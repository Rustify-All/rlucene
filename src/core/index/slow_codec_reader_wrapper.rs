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
use crate::core::codecs::fields_producer::FieldsProducer;
use crate::core::codecs::hnsw::hnsw_graph_provider::HnswGraphProvider;
use crate::core::codecs::knn_vectors_reader::KnnVectorsReader;
use crate::core::codecs::norms_producer::NormsProducer;
use crate::core::codecs::points_reader::PointsReader;
use crate::core::codecs::stored_fields_reader::StoredFieldsReader;
use crate::core::codecs::stored_fields_writer::StoredFieldsWriter;
use crate::core::codecs::term_vectors_reader::{DefaultTermVectorsReader, TermVectorsReader};
use crate::core::index::codec_reader::{CodecReader, StoredFieldsType, TermVectorsType};
use crate::core::index::dummy::dummy_byte_vector_values::DummyByteVectorValues;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::fields::Fields;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::index_reader::{IndexReader, IndexReaderBase, LeafReaderContextKind};
use crate::core::index::leaf_metadata::LeafMetaData;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::stored_field_visitor::StoredFieldVisitor;
use crate::core::index::stored_fields::{RawStoredFieldsReader, StoredFields};
use crate::core::index::term::Term;
use crate::core::index::term_vectors::{RawTermVectors, TermVectors};
use crate::core::search::knn_collector::KnnCollector;
use crate::core::util::bits::Bits;
use crate::core::util::clone::TryClone;
use crate::core::util::close::CloseableRef;
use crate::core::util::dummy::dummy_hnsw_graph::DummyHnswGraph;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::iterator::IteratorExt;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

/// Wraps arbitrary readers for merging. Note that this can cause slow
/// and memory-intensive merges. Consider using
/// [`FilterCodecReader`](crate::core::index::filter_codec_reader::FilterCodecReader)
/// instead.
pub struct SlowCodecReaderWrapper;
impl SlowCodecReaderWrapper {
  /// Returns a [CodecReader] view of reader.
  ///
  /// If `reader` is already a [CodecReader], it is returned directly.
  /// Otherwise, a (slow) view is returned.
  pub fn wrap_leaf_reader<LR>(reader: LR) -> CodecReaderImpl<LR>
  where
    LR: LeafReader,
  {
    CodecReaderImpl::new(reader)
  }
}

pub struct CodecReaderImpl<LR> {
  reader: LR,
  index_base: IndexReaderBase,
}

impl<LR> Clone for CodecReaderImpl<LR>
where
  LR: LeafReader + Clone,
{
  fn clone(&self) -> Self {
    Self::new(self.reader.clone())
  }
}

impl<LR> CodecReaderImpl<LR> {
  pub(crate) fn new(reader: LR) -> Self {
    Self {
      reader,
      index_base: IndexReaderBase::new(),
    }
  }
}

impl<LR> LeafReader for CodecReaderImpl<LR>
where
  LR: LeafReader + Clone,
{
  type CacheHelper = LR::CacheHelper;

  fn get_core_cache_helper(&self) -> Result<Option<Self::CacheHelper>> {
    self.reader.get_core_cache_helper()
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

  type FloatVectorValues =
    <<Self as CodecReader>::KnnVectorsReader as KnnVectorsReader>::FloatVectorValues;

  fn get_float_vector_values(&self, field: &str) -> Result<Option<Self::FloatVectorValues>> {
    CodecReader::get_float_vector_values(self, field)
  }

  type ByteVectorValues =
    <<Self as CodecReader>::KnnVectorsReader as KnnVectorsReader>::ByteVectorValues;

  fn get_byte_vector_values(&self, field: &str) -> Result<Option<Self::ByteVectorValues>> {
    CodecReader::get_byte_vector_values(self, field)
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
    CodecReader::search_nearest_vectors_f32(self, field, target, knn_collector, accept_docs)
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
    CodecReader::search_nearest_vectors_u8(self, field, target, knn_collector, accept_docs)
  }

  fn get_field_infos(&self) -> Result<Arc<FieldInfos>> {
    self.reader.get_field_infos()
  }

  type Bits = LR::Bits;

  fn get_live_docs(&self) -> Result<Option<Self::Bits>> {
    self.reader.get_live_docs()
  }

  type PointValues = <<Self as CodecReader>::PointsReader as PointsReader>::PointValuesType;

  fn get_point_values(&self, field: &str) -> Result<Option<Self::PointValues>> {
    CodecReader::get_point_values(self, field)
  }

  fn get_metadata(&self) -> Result<&LeafMetaData> {
    self.reader.get_metadata()
  }
}

impl<LR> IndexReader for CodecReaderImpl<LR>
where
  LR: LeafReader + Clone,
{
  type ContextKind = LeafReaderContextKind;

  type TermVectors = TermVectorsType<<Self as CodecReader>::TermVectorsReader>;

  fn term_vectors(&self) -> Result<Self::TermVectors> {
    CodecReader::term_vectors(self)
  }

  fn max_doc(&self) -> Result<i32> {
    self.reader.max_doc()
  }

  fn num_docs(&self) -> Result<i32> {
    self.reader.num_docs()
  }

  type StoredFields = StoredFieldsType<<Self as CodecReader>::StoredFieldsReader>;

  fn stored_fields(&self) -> Result<Self::StoredFields> {
    CodecReader::stored_fields(self)
  }

  fn do_close(&self) -> Result<()> {
    Ok(())
  }

  type ReaderCacheHelper = LR::ReaderCacheHelper;

  fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
    self.reader.get_reader_cache_helper()
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

impl<LR> Display for CodecReaderImpl<LR>
where
  LR: LeafReader,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "SlowCodecReaderWrapper({})", self.reader)
  }
}

impl<LR> CodecReader for CodecReaderImpl<LR>
where
  LR: LeafReader + Clone,
{
  type StoredFieldsReader = StoredFieldsReaderImpl<LR>;
  type TermVectorsReader = TermVectorsReaderImpl<LR>;
  type NormsProducer = NormsProducerImpl<LR>;
  type DocValuesProducer = DocValuesProducerImpl<LR>;
  type FieldsProducer = FieldsProducerImpl<LR>;
  type PointsReader = PointsReaderImpl<LR>;
  type KnnVectorsReader = KnnVectorsReaderImpl<LR>;

  fn get_fields_reader(&self) -> Result<Option<Self::StoredFieldsReader>> {
    self.reader.ensure_open()?;
    Ok(Some(reader_to_stored_fields_reader(self.reader.clone())?))
  }

  fn get_term_vectors_reader(&self) -> Result<Option<Self::TermVectorsReader>> {
    self.reader.ensure_open()?;
    Ok(Some(reader_to_term_vectors_reader(self.reader.clone())?))
  }

  fn get_norms_reader(&self) -> Result<Option<Self::NormsProducer>> {
    self.reader.ensure_open()?;
    Ok(Some(reader_to_norms_producer(self.reader.clone())))
  }

  fn get_doc_values_reader(&self) -> Result<Option<Self::DocValuesProducer>> {
    self.reader.ensure_open()?;
    Ok(Some(reader_to_doc_values_producer(self.reader.clone())))
  }

  fn get_postings_reader(&self) -> Result<Option<Self::FieldsProducer>> {
    self.reader.ensure_open()?;
    Ok(Some(reader_to_fields_producer(self.reader.clone())?))
  }

  fn get_points_reader(&self) -> Result<Option<Self::PointsReader>> {
    Ok(Some(point_values_to_reader(self.reader.clone())))
  }

  fn get_vector_reader(&self) -> Result<Option<Self::KnnVectorsReader>> {
    Ok(Some(reader_to_vector_reader(self.reader.clone())))
  }
}

fn point_values_to_reader<LR>(reader: LR) -> PointsReaderImpl<LR>
where
  LR: LeafReader,
{
  PointsReaderImpl::new(reader)
}

pub struct PointsReaderImpl<LR> {
  reader: LR,
}
impl<LR> PointsReaderImpl<LR> {
  fn new(reader: LR) -> Self {
    Self { reader }
  }
}

impl<LR> CloseableRef for PointsReaderImpl<LR> {}

impl<LR> PointsReader for PointsReaderImpl<LR>
where
  LR: LeafReader,
{
  fn check_integrity(&self) -> Result<()> {
    self.reader.check_integrity()
  }

  type PointValuesType = LR::PointValues;

  fn get_values(&self, field: &str) -> Result<Option<Self::PointValuesType>> {
    self.reader.get_point_values(field)
  }
}

fn reader_to_norms_producer<LR>(reader: LR) -> NormsProducerImpl<LR>
where
  LR: LeafReader,
{
  NormsProducerImpl::new(reader)
}

pub struct NormsProducerImpl<LR> {
  reader: LR,
}

impl<LR> NormsProducerImpl<LR> {
  fn new(reader: LR) -> Self {
    Self { reader }
  }
}

impl<LR> CloseableRef for NormsProducerImpl<LR> {}

impl<LR> NormsProducer for NormsProducerImpl<LR>
where
  LR: LeafReader,
{
  type MergeInstance = Self;
  type NumericDocValues = LR::NormNumericDocValues;

  fn get_norms(&self, field: &Arc<FieldInfo>) -> Result<Self::NumericDocValues> {
    self
      .reader
      .get_norm_values(&field.name)?
      .ok_or_else(|| LuceneError::illegal_state("norm numeric doc value is None"))
  }

  fn check_integrity(&self) -> Result<()> {
    Ok(())
  }
}

fn reader_to_doc_values_producer<LR>(reader: LR) -> DocValuesProducerImpl<LR>
where
  LR: LeafReader,
{
  DocValuesProducerImpl::new(reader)
}

pub struct DocValuesProducerImpl<LR> {
  reader: LR,
}

impl<LR> DocValuesProducerImpl<LR> {
  fn new(reader: LR) -> Self {
    Self { reader }
  }
}

impl<LR> CloseableRef for DocValuesProducerImpl<LR> {}

impl<LR> DocValuesProducer for DocValuesProducerImpl<LR>
where
  LR: LeafReader,
{
  type NumericDocValues = LR::NumericDocValues;
  fn get_numeric(&self, field: &Arc<FieldInfo>) -> Result<Self::NumericDocValues> {
    self
      .reader
      .get_numeric_doc_values(&field.name)?
      .ok_or_else(|| LuceneError::illegal_state("numeric doc values is None"))
  }
  type BinaryDocValues = LR::BinaryDocValues;
  fn get_binary(&self, field: &Arc<FieldInfo>) -> Result<Self::BinaryDocValues> {
    self
      .reader
      .get_binary_doc_values(&field.name)?
      .ok_or_else(|| LuceneError::illegal_state("binary doc values is None"))
  }
  type SortedDocValues = LR::SortedDocValues;
  fn get_sorted(&self, field: &Arc<FieldInfo>) -> Result<Self::SortedDocValues> {
    self
      .reader
      .get_sorted_doc_values(&field.name)?
      .ok_or_else(|| LuceneError::illegal_state("sorted doc values is None"))
  }

  type SortedNumericDocValues = LR::SortedNumericDocValues;

  fn get_sorted_numeric(&self, field: &Arc<FieldInfo>) -> Result<Self::SortedNumericDocValues> {
    self
      .reader
      .get_sorted_numeric_doc_values(&field.name)?
      .ok_or_else(|| LuceneError::illegal_state("sorted numeric doc values is None"))
  }

  type SortedSetDocValues = LR::SortedSetDocValues;

  fn get_sorted_set(&self, field: &Arc<FieldInfo>) -> Result<Self::SortedSetDocValues> {
    self
      .reader
      .get_sorted_set_doc_values(&field.name)?
      .ok_or_else(|| LuceneError::illegal_state("sorted set doc values is None"))
  }

  type DocValuesSkipper = LR::DocValuesSkipper;

  fn get_skipper(&self, field: &Arc<FieldInfo>) -> Result<Option<Self::DocValuesSkipper>> {
    self.reader.get_doc_values_skipper(&field.name)
  }

  fn check_integrity(&self) -> Result<()> {
    Ok(())
  }
}
fn reader_to_stored_fields_reader<LR>(reader: LR) -> Result<StoredFieldsReaderImpl<LR>>
where
  LR: LeafReader + Clone,
{
  let stored_fields = reader.stored_fields()?;

  Ok(StoredFieldsReaderImpl::new(reader, stored_fields))
}

pub struct StoredFieldsReaderImpl<LR>
where
  LR: LeafReader + Clone,
{
  reader: LR,
  stored_fields: LR::StoredFields,
}

impl<LR> StoredFieldsReaderImpl<LR>
where
  LR: LeafReader + Clone,
{
  fn new(reader: LR, stored_fields: LR::StoredFields) -> Self {
    Self {
      reader,
      stored_fields,
    }
  }
}

impl<LR> StoredFields for StoredFieldsReaderImpl<LR>
where
  LR: Clone + LeafReader,
{
  fn prefetch(&mut self, doc_id: i32) -> Result<()> {
    self.stored_fields.prefetch(doc_id)
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
      .stored_fields
      .document_with_visitor(doc_id, visitor, writer)
  }
}

impl<LR> RawStoredFieldsReader for StoredFieldsReaderImpl<LR>
where
  LR: Clone + LeafReader,
  LR::StoredFields: RawStoredFieldsReader,
{
  type IndexInput = <LR::StoredFields as RawStoredFieldsReader>::IndexInput;
}

impl<LR> TryClone for StoredFieldsReaderImpl<LR>
where
  LR: Clone + LeafReader,
{
  fn try_clone(&self) -> Result<Self>
  where
    Self: Sized,
  {
    reader_to_stored_fields_reader(self.reader.clone())
  }
}

impl<LR> CloseableRef for StoredFieldsReaderImpl<LR> where LR: LeafReader + Clone {}

impl<LR> StoredFieldsReader for StoredFieldsReaderImpl<LR>
where
  LR: LeafReader + Clone,
{
  fn check_integrity(&self) -> Result<()> {
    Ok(())
  }
}
fn reader_to_term_vectors_reader<LR>(reader: LR) -> Result<TermVectorsReaderImpl<LR>>
where
  LR: LeafReader + Clone,
{
  let term_vectors = reader.term_vectors()?;

  Ok(TermVectorsReaderImpl::new(reader, term_vectors))
}

pub struct TermVectorsReaderImpl<LR>
where
  LR: LeafReader,
{
  reader: LR,
  term_vectors: LR::TermVectors,
}

impl<LR> TermVectorsReaderImpl<LR>
where
  LR: LeafReader,
{
  fn new(reader: LR, term_vectors: LR::TermVectors) -> Self {
    Self {
      reader,
      term_vectors,
    }
  }
}

impl<LR> TermVectors for TermVectorsReaderImpl<LR>
where
  LR: LeafReader,
{
  fn prefetch(&mut self, doc_id: i32) -> Result<()> {
    self.term_vectors.prefetch(doc_id)
  }

  type Fields = <LR::TermVectors as TermVectors>::Fields;

  fn get(&mut self, doc: i32) -> Result<Option<Self::Fields>> {
    self.term_vectors.get(doc)
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

impl<LR> RawTermVectors for TermVectorsReaderImpl<LR>
where
  LR: LeafReader,
{
  type IndexInput = <LR::TermVectors as RawTermVectors>::IndexInput;

  fn raw_term_vectors_mut(&mut self) -> Result<&mut DefaultTermVectorsReader<Self::IndexInput>> {
    Err(LuceneError::unsupported_operation(
      "raw term vectors are not available for SlowCodecReaderWrapper",
    ))
  }

  fn raw_term_vectors(&self) -> Result<&DefaultTermVectorsReader<Self::IndexInput>> {
    Err(LuceneError::unsupported_operation(
      "raw term vectors are not available for SlowCodecReaderWrapper",
    ))
  }
}

impl<LR> TryClone for TermVectorsReaderImpl<LR>
where
  LR: LeafReader + Clone,
{
  fn try_clone(&self) -> Result<Self>
  where
    Self: Sized,
  {
    reader_to_term_vectors_reader(self.reader.clone())
  }
}

impl<LR> CloseableRef for TermVectorsReaderImpl<LR> where LR: LeafReader + Clone {}

impl<LR> TermVectorsReader for TermVectorsReaderImpl<LR>
where
  LR: LeafReader + Clone,
{
  fn check_integrity(&self) -> Result<()> {
    Ok(())
  }
}

fn reader_to_fields_producer<LR>(reader: LR) -> Result<FieldsProducerImpl<LR>>
where
  LR: LeafReader,
{
  let field_infos = reader.get_field_infos()?;
  if !field_infos.has_postings() {
    return Ok(FieldsProducerImpl::new(reader, Vec::new()));
  }
  let mut indexed_fields = Vec::with_capacity(field_infos.size());

  for field_info in field_infos.iter() {
    if *field_info.get_index_options() != IndexOptions::None {
      indexed_fields.push(Arc::clone(field_info));
    }
  }

  indexed_fields.sort_unstable_by(|a, b| a.name.cmp(&b.name));

  Ok(FieldsProducerImpl::new(reader, indexed_fields))
}

pub struct FieldNamesIterator<'a>(std::slice::Iter<'a, Arc<FieldInfo>>);

impl<'a> IteratorExt for FieldNamesIterator<'a> {
  type Item = &'a String;

  fn next(&mut self) -> Result<Option<Self::Item>> {
    Ok(self.0.next().map(|field| &field.name))
  }

  fn has_next(&self) -> Result<bool> {
    Ok(!self.0.as_slice().is_empty())
  }
}

pub struct FieldsProducerImpl<LR> {
  reader: LR,
  indexed_fields: Vec<Arc<FieldInfo>>,
}

impl<LR> FieldsProducerImpl<LR> {
  fn new(reader: LR, indexed_fields: Vec<Arc<FieldInfo>>) -> Self {
    Self {
      reader,
      indexed_fields,
    }
  }
}

impl<LR> CloseableRef for FieldsProducerImpl<LR> {}

impl<LR> Fields for FieldsProducerImpl<LR>
where
  LR: LeafReader,
{
  type FieldIter<'a>
    = FieldNamesIterator<'a>
  where
    LR: 'a;

  fn iterator(&self) -> Result<Self::FieldIter<'_>> {
    Ok(FieldNamesIterator(self.indexed_fields.iter()))
  }

  type Terms = <LR as LeafReader>::Terms;

  fn terms(&self, field: &str) -> Result<Option<LR::Terms>> {
    self.reader.terms(field)
  }

  fn size(&self) -> Result<i32> {
    Ok(self.indexed_fields.len() as i32)
  }
}

impl<LR> FieldsProducer for FieldsProducerImpl<LR>
where
  LR: LeafReader,
{
  fn check_integrity(&self) -> Result<()> {
    Ok(())
  }
}

pub fn reader_to_vector_reader<LR>(reader: LR) -> KnnVectorsReaderImpl<LR>
where
  LR: LeafReader,
{
  KnnVectorsReaderImpl::new(reader)
}

pub struct KnnVectorsReaderImpl<LR> {
  reader: LR,
}
impl<LR> KnnVectorsReaderImpl<LR> {
  fn new(reader: LR) -> Self {
    Self { reader }
  }
}

impl<LR> CloseableRef for KnnVectorsReaderImpl<LR> {}

impl<LR> HnswGraphProvider for KnnVectorsReaderImpl<LR>
where
  LR: LeafReader,
{
  type HnswGraph = DummyHnswGraph;
}
impl<LR> KnnVectorsReader for KnnVectorsReaderImpl<LR>
where
  LR: LeafReader,
{
  fn check_integrity(&self) -> Result<()> {
    Ok(())
  }

  type FloatVectorValues = LR::FloatVectorValues;

  fn get_float_vector_values(&self, field: &str) -> Result<Self::FloatVectorValues> {
    match self.reader.get_float_vector_values(field)? {
      Some(v) => Ok(v),
      None => Err(LuceneError::illegal_state(
        "FloatVectorValues from leaf reader does not support get_float_vector_values ",
      )),
    }
  }

  type ByteVectorValues = LR::ByteVectorValues;

  fn get_byte_vector_values(&self, field: &str) -> Result<Self::ByteVectorValues> {
    match self.reader.get_byte_vector_values(field)? {
      Some(v) => Ok(v),
      None => Err(LuceneError::illegal_state(
        "ByteVectorValues from leaf reader does not support get_float_vector_values ",
      )),
    }
  }

  type QuantizedByteVectorValues = DummyByteVectorValues;

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
    self
      .reader
      .search_nearest_vectors_f32(field, target, knn_collector, accept_docs)
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
    self
      .reader
      .search_nearest_vectors_u8(field, target, knn_collector, accept_docs)
  }
}
