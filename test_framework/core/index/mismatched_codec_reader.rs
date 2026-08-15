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
use crate::core::codecs::norms_producer::NormsProducer;
use crate::core::codecs::stored_fields_reader::StoredFieldsReader;
use crate::core::codecs::stored_fields_writer::StoredFieldsWriter;
use crate::core::index::codec_reader::{CodecReader, TermVectorsType};
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::index_reader::{IndexReader, IndexReaderBase, LeafReaderContextKind};
use crate::core::index::leaf_metadata::LeafMetaData;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::stored_field_visitor::StoredFieldVisitor;
use crate::core::index::stored_fields::{RawStoredFieldsReader, StoredFields};
use crate::core::index::term::Term;
use crate::core::search::knn_collector::KnnCollector;
use crate::core::util::bits::Bits;
use crate::core::util::clone::TryClone;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::test_framework::core::index::mismatched_leaf_reader::{
  MismatchedVisitor, shuffle_infos,
};
use rand::Rng;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
/// Shuffles field numbers around to try to trip bugs where field numbers are assumed to always be consistent across segments.
pub struct MismatchedCodecReader<CR> {
  in_: CR,
  shuffled: Arc<FieldInfos>,
}

impl<CR> MismatchedCodecReader<CR>
where
  CR: CodecReader,
{
  pub fn new<R>(reader: CR, random: &mut R) -> Result<Self>
  where
    R: Rng + ?Sized,
  {
    let shuffled = shuffle_infos(reader.get_field_infos()?.as_ref(), random)?;
    Ok(Self {
      in_: reader,
      shuffled: Arc::new(shuffled),
    })
  }
}

impl<CR> Clone for MismatchedCodecReader<CR>
where
  CR: CodecReader + Clone,
{
  fn clone(&self) -> Self {
    Self {
      in_: self.in_.clone(),
      shuffled: self.shuffled.clone(),
    }
  }
}

impl<CR> Display for MismatchedCodecReader<CR>
where
  CR: CodecReader,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "MismatchedCodecReader({})", self.in_)
  }
}

impl<CR> IndexReader for MismatchedCodecReader<CR>
where
  CR: CodecReader,
{
  type ContextKind = LeafReaderContextKind;

  type TermVectors = TermVectorsType<CR::TermVectorsReader>;

  fn term_vectors(&self) -> Result<Self::TermVectors> {
    CodecReader::term_vectors(self)
  }

  fn max_doc(&self) -> Result<i32> {
    self.in_.max_doc()
  }

  fn num_docs(&self) -> Result<i32> {
    self.in_.num_docs()
  }

  type StoredFields = crate::core::index::codec_reader::StoredFieldsType<
    MismatchedStoredFieldsReader<CR::StoredFieldsReader>,
  >;

  fn stored_fields(&self) -> Result<Self::StoredFields> {
    CodecReader::stored_fields(self)
  }

  fn do_close(&self) -> Result<()> {
    self.in_.do_close()
  }

  type ReaderCacheHelper = CR::ReaderCacheHelper;

  fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
    self.in_.get_reader_cache_helper()
  }

  fn doc_freq(&self, term: &Term) -> Result<i32> {
    IndexReader::doc_freq(&self.in_, term)
  }

  fn total_term_freq(&self, term: &Term) -> Result<i64> {
    self.in_.total_term_freq(term)
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
    self.in_.index_base()
  }
}

impl<CR> LeafReader for MismatchedCodecReader<CR>
where
  CR: CodecReader,
{
  type CacheHelper = CR::CacheHelper;

  fn get_core_cache_helper(&self) -> Result<Option<Self::CacheHelper>> {
    self.in_.get_core_cache_helper()
  }

  type Terms = CR::Terms;

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    LeafReader::terms(&self.in_, field)
  }

  type NumericDocValues =
    <MismatchedDocValuesProducer<CR::DocValuesProducer> as DocValuesProducer>::NumericDocValues;

  fn get_numeric_doc_values(&self, field: &str) -> Result<Option<Self::NumericDocValues>> {
    CodecReader::get_numeric_doc_values(self, field)
  }

  type BinaryDocValues =
    <MismatchedDocValuesProducer<CR::DocValuesProducer> as DocValuesProducer>::BinaryDocValues;

  fn get_binary_doc_values(&self, field: &str) -> Result<Option<Self::BinaryDocValues>> {
    CodecReader::get_binary_doc_values(self, field)
  }

  type SortedDocValues =
    <MismatchedDocValuesProducer<CR::DocValuesProducer> as DocValuesProducer>::SortedDocValues;

  fn get_sorted_doc_values(&self, field: &str) -> Result<Option<Self::SortedDocValues>> {
    CodecReader::get_sorted_doc_values(self, field)
  }

  type SortedNumericDocValues = <MismatchedDocValuesProducer<CR::DocValuesProducer> as DocValuesProducer>::SortedNumericDocValues;

  fn get_sorted_numeric_doc_values(
    &self,
    field: &str,
  ) -> Result<Option<Self::SortedNumericDocValues>> {
    CodecReader::get_sorted_numeric_doc_values(self, field)
  }

  type SortedSetDocValues =
    <MismatchedDocValuesProducer<CR::DocValuesProducer> as DocValuesProducer>::SortedSetDocValues;

  fn get_sorted_set_doc_values(&self, field: &str) -> Result<Option<Self::SortedSetDocValues>> {
    CodecReader::get_sorted_set_doc_values(self, field)
  }

  type NormNumericDocValues =
    <MismatchedNormsProducer<CR::NormsProducer> as NormsProducer>::NumericDocValues;

  fn get_norm_values(&self, field: &str) -> Result<Option<Self::NormNumericDocValues>> {
    CodecReader::get_norm_values(self, field)
  }

  type DocValuesSkipper =
    <MismatchedDocValuesProducer<CR::DocValuesProducer> as DocValuesProducer>::DocValuesSkipper;

  fn get_doc_values_skipper(&self, field: &str) -> Result<Option<Self::DocValuesSkipper>> {
    CodecReader::get_doc_values_skipper(self, field)
  }

  type FloatVectorValues = CR::FloatVectorValues;

  fn get_float_vector_values(&self, field: &str) -> Result<Option<Self::FloatVectorValues>> {
    LeafReader::get_float_vector_values(&self.in_, field)
  }

  type ByteVectorValues = CR::ByteVectorValues;

  fn get_byte_vector_values(&self, field: &str) -> Result<Option<Self::ByteVectorValues>> {
    LeafReader::get_byte_vector_values(&self.in_, field)
  }

  fn search_nearest_vectors_f32<B, K>(
    &self,
    field: &str,
    target: Vec<f32>,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
  {
    LeafReader::search_nearest_vectors_f32(&self.in_, field, target, knn_collector, accept_docs)
  }

  fn search_nearest_vectors_u8<B, K>(
    &self,
    field: &str,
    target: Vec<u8>,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
  {
    LeafReader::search_nearest_vectors_u8(&self.in_, field, target, knn_collector, accept_docs)
  }

  fn get_field_infos(&self) -> Result<Arc<FieldInfos>> {
    Ok(self.shuffled.clone())
  }

  type Bits = CR::Bits;

  fn get_live_docs(&self) -> Result<Option<Self::Bits>> {
    self.in_.get_live_docs()
  }

  type PointValues = CR::PointValues;

  fn get_point_values(&self, field: &str) -> Result<Option<Self::PointValues>> {
    LeafReader::get_point_values(&self.in_, field)
  }

  fn check_integrity(&self) -> Result<()> {
    self.in_.check_integrity()
  }

  fn get_metadata(&self) -> Result<&LeafMetaData> {
    self.in_.get_metadata()
  }
}

impl<CR> CodecReader for MismatchedCodecReader<CR>
where
  CR: CodecReader,
{
  type StoredFieldsReader = MismatchedStoredFieldsReader<CR::StoredFieldsReader>;
  type TermVectorsReader = CR::TermVectorsReader;
  type NormsProducer = MismatchedNormsProducer<CR::NormsProducer>;
  type DocValuesProducer = MismatchedDocValuesProducer<CR::DocValuesProducer>;
  type FieldsProducer = CR::FieldsProducer;
  type PointsReader = CR::PointsReader;
  type KnnVectorsReader = CR::KnnVectorsReader;

  fn get_fields_reader(&self) -> Result<Option<Self::StoredFieldsReader>> {
    Ok(
      self
        .in_
        .get_fields_reader()?
        .map(|inner| MismatchedStoredFieldsReader::new(inner, self.shuffled.clone())),
    )
  }

  fn get_term_vectors_reader(&self) -> Result<Option<Self::TermVectorsReader>> {
    self.in_.get_term_vectors_reader()
  }

  fn get_norms_reader(&self) -> Result<Option<Self::NormsProducer>> {
    let orig = self.in_.get_field_infos()?;
    Ok(
      self
        .in_
        .get_norms_reader()?
        .map(|inner| MismatchedNormsProducer::new(inner, self.shuffled.clone(), orig)),
    )
  }

  fn get_doc_values_reader(&self) -> Result<Option<Self::DocValuesProducer>> {
    let orig = self.in_.get_field_infos()?;
    Ok(
      self
        .in_
        .get_doc_values_reader()?
        .map(|inner| MismatchedDocValuesProducer::new(inner, self.shuffled.clone(), orig)),
    )
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

pub struct MismatchedStoredFieldsReader<SFR> {
  in_: SFR,
  shuffled: Arc<FieldInfos>,
}

impl<SFR> MismatchedStoredFieldsReader<SFR>
where
  SFR: StoredFieldsReader,
{
  fn new(in_: SFR, shuffled: Arc<FieldInfos>) -> Self {
    Self { in_, shuffled }
  }
}

impl<SFR> TryClone for MismatchedStoredFieldsReader<SFR>
where
  SFR: StoredFieldsReader,
{
  fn try_clone(&self) -> Result<Self>
  where
    Self: Sized,
  {
    Ok(Self::new(self.in_.try_clone()?, self.shuffled.clone()))
  }
}

impl<SFR> RawStoredFieldsReader for MismatchedStoredFieldsReader<SFR>
where
  SFR: StoredFieldsReader + RawStoredFieldsReader,
{
  type IndexInput = SFR::IndexInput;
}

impl<SFR> StoredFields for MismatchedStoredFieldsReader<SFR>
where
  SFR: StoredFieldsReader,
{
  fn document_with_visitor<S>(
    &mut self,
    doc_id: i32,
    visitor: &mut impl StoredFieldVisitor,
    writer: Option<&mut S>,
  ) -> Result<()>
  where
    S: StoredFieldsWriter,
  {
    let mut mismatched_visitor = MismatchedVisitor::new(visitor, self.shuffled.clone());
    self
      .in_
      .document_with_visitor(doc_id, &mut mismatched_visitor, writer)
  }
}

impl<SFR> CloseableRef for MismatchedStoredFieldsReader<SFR>
where
  SFR: StoredFieldsReader,
{
  fn close(&self) -> Result<()> {
    self.in_.close()
  }
}

impl<SFR> StoredFieldsReader for MismatchedStoredFieldsReader<SFR>
where
  SFR: StoredFieldsReader,
{
  fn check_integrity(&self) -> Result<()> {
    self.in_.check_integrity()
  }
}

pub struct MismatchedDocValuesProducer<DVP> {
  in_: DVP,
  shuffled: Arc<FieldInfos>,
  orig: Arc<FieldInfos>,
}

impl<DVP> MismatchedDocValuesProducer<DVP>
where
  DVP: DocValuesProducer,
{
  fn new(in_: DVP, shuffled: Arc<FieldInfos>, orig: Arc<FieldInfos>) -> Self {
    Self {
      in_,
      shuffled,
      orig,
    }
  }

  fn remap_field_info(&self, field: &Arc<FieldInfo>) -> Result<Arc<FieldInfo>> {
    let shuffled = self
      .shuffled
      .field_info_by_name(&field.name)?
      .ok_or_else(|| {
        LuceneError::illegal_state(format!("missing shuffled field info for {}", field.name))
      })?;
    assert_eq!(shuffled.number, field.number);
    self.orig.field_info_by_name(&field.name)?.ok_or_else(|| {
      LuceneError::illegal_state(format!("missing original field info for {}", field.name))
    })
  }
}

impl<DVP> CloseableRef for MismatchedDocValuesProducer<DVP>
where
  DVP: DocValuesProducer,
{
  fn close(&self) -> Result<()> {
    self.in_.close()
  }
}

impl<DVP> DocValuesProducer for MismatchedDocValuesProducer<DVP>
where
  DVP: DocValuesProducer,
{
  type NumericDocValues = DVP::NumericDocValues;

  fn get_numeric(&self, field: &Arc<FieldInfo>) -> Result<Self::NumericDocValues> {
    self.in_.get_numeric(&self.remap_field_info(field)?)
  }

  type BinaryDocValues = DVP::BinaryDocValues;

  fn get_binary(&self, field: &Arc<FieldInfo>) -> Result<Self::BinaryDocValues> {
    self.in_.get_binary(&self.remap_field_info(field)?)
  }

  type SortedDocValues = DVP::SortedDocValues;

  fn get_sorted(&self, field: &Arc<FieldInfo>) -> Result<Self::SortedDocValues> {
    self.in_.get_sorted(&self.remap_field_info(field)?)
  }

  type SortedNumericDocValues = DVP::SortedNumericDocValues;

  fn get_sorted_numeric(&self, field: &Arc<FieldInfo>) -> Result<Self::SortedNumericDocValues> {
    self.in_.get_sorted_numeric(&self.remap_field_info(field)?)
  }

  type SortedSetDocValues = DVP::SortedSetDocValues;

  fn get_sorted_set(&self, field: &Arc<FieldInfo>) -> Result<Self::SortedSetDocValues> {
    self.in_.get_sorted_set(&self.remap_field_info(field)?)
  }

  type DocValuesSkipper = DVP::DocValuesSkipper;

  fn get_skipper(&self, field: &Arc<FieldInfo>) -> Result<Option<Self::DocValuesSkipper>> {
    self.in_.get_skipper(&self.remap_field_info(field)?)
  }

  fn check_integrity(&self) -> Result<()> {
    self.in_.check_integrity()
  }
}

pub struct MismatchedNormsProducer<NP> {
  in_: NP,
  shuffled: Arc<FieldInfos>,
  orig: Arc<FieldInfos>,
}

impl<NP> MismatchedNormsProducer<NP>
where
  NP: NormsProducer,
{
  fn new(in_: NP, shuffled: Arc<FieldInfos>, orig: Arc<FieldInfos>) -> Self {
    Self {
      in_,
      shuffled,
      orig,
    }
  }

  fn remap_field_info(&self, field: &Arc<FieldInfo>) -> Result<Arc<FieldInfo>> {
    let shuffled = self
      .shuffled
      .field_info_by_name(&field.name)?
      .ok_or_else(|| {
        LuceneError::illegal_state(format!("missing shuffled field info for {}", field.name))
      })?;
    assert_eq!(shuffled.number, field.number);
    self.orig.field_info_by_name(&field.name)?.ok_or_else(|| {
      LuceneError::illegal_state(format!("missing original field info for {}", field.name))
    })
  }
}

impl<NP> CloseableRef for MismatchedNormsProducer<NP>
where
  NP: NormsProducer,
{
  fn close(&self) -> Result<()> {
    self.in_.close()
  }
}

impl<NP> NormsProducer for MismatchedNormsProducer<NP>
where
  NP: NormsProducer,
{
  type NumericDocValues = NP::NumericDocValues;

  fn get_norms(&self, field: &Arc<FieldInfo>) -> Result<Self::NumericDocValues> {
    self.in_.get_norms(&self.remap_field_info(field)?)
  }

  fn check_integrity(&self) -> Result<()> {
    self.in_.check_integrity()
  }
}
