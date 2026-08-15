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
use crate::core::index::codec_reader::{CodecReader, StoredFieldsType, TermVectorsType};
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::index_reader::{IndexReader, IndexReaderBase, LeafReaderContextKind};
use crate::core::index::leaf_metadata::LeafMetaData;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::term::Term;
use crate::core::search::knn_collector::KnnCollector;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::Result;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

/// [`CodecReader`] wrapper that performs all reads using the merging instance of the index
/// formats.
pub struct MergingCodecReader<CR> {
  in_: CR,
}

impl<CR> MergingCodecReader<CR>
where
  CR: CodecReader,
{
  /// Wrap the given instance.
  pub fn new(in_: CR) -> Self {
    Self { in_ }
  }
}

impl<CR> Clone for MergingCodecReader<CR>
where
  CR: CodecReader + Clone,
{
  fn clone(&self) -> Self {
    Self::new(self.in_.clone())
  }
}

impl<CR> Display for MergingCodecReader<CR>
where
  CR: CodecReader,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "MergingCodecReader({})", self.in_)
  }
}

impl<CR> IndexReader for MergingCodecReader<CR>
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

  type StoredFields = StoredFieldsType<CR::StoredFieldsReader>;

  fn stored_fields(&self) -> Result<Self::StoredFields> {
    CodecReader::stored_fields(self)
  }

  fn do_close(&self) -> Result<()> {
    self.in_.do_close()
  }

  type ReaderCacheHelper = CR::ReaderCacheHelper;

  fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
    // same content, we can delegate
    self.in_.get_reader_cache_helper()
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
    self.in_.index_base()
  }
}

impl<CR> LeafReader for MergingCodecReader<CR>
where
  CR: CodecReader,
{
  type CacheHelper = CR::CacheHelper;

  fn get_core_cache_helper(&self) -> Result<Option<Self::CacheHelper>> {
    // same content, we can delegate
    self.in_.get_core_cache_helper()
  }

  type Terms = CR::Terms;

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    LeafReader::terms(&self.in_, field)
  }

  type NumericDocValues = <CR::DocValuesProducer as DocValuesProducer>::NumericDocValues;

  fn get_numeric_doc_values(&self, field: &str) -> Result<Option<Self::NumericDocValues>> {
    CodecReader::get_numeric_doc_values(self, field)
  }

  type BinaryDocValues = <CR::DocValuesProducer as DocValuesProducer>::BinaryDocValues;

  fn get_binary_doc_values(&self, field: &str) -> Result<Option<Self::BinaryDocValues>> {
    CodecReader::get_binary_doc_values(self, field)
  }

  type SortedDocValues = <CR::DocValuesProducer as DocValuesProducer>::SortedDocValues;

  fn get_sorted_doc_values(&self, field: &str) -> Result<Option<Self::SortedDocValues>> {
    CodecReader::get_sorted_doc_values(self, field)
  }

  type SortedNumericDocValues =
    <CR::DocValuesProducer as DocValuesProducer>::SortedNumericDocValues;

  fn get_sorted_numeric_doc_values(
    &self,
    field: &str,
  ) -> Result<Option<Self::SortedNumericDocValues>> {
    CodecReader::get_sorted_numeric_doc_values(self, field)
  }

  type SortedSetDocValues = <CR::DocValuesProducer as DocValuesProducer>::SortedSetDocValues;

  fn get_sorted_set_doc_values(&self, field: &str) -> Result<Option<Self::SortedSetDocValues>> {
    CodecReader::get_sorted_set_doc_values(self, field)
  }

  type NormNumericDocValues = <CR::NormsProducer as NormsProducer>::NumericDocValues;

  fn get_norm_values(&self, field: &str) -> Result<Option<Self::NormNumericDocValues>> {
    CodecReader::get_norm_values(self, field)
  }

  type DocValuesSkipper = <CR::DocValuesProducer as DocValuesProducer>::DocValuesSkipper;

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
    self.in_.get_field_infos()
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

impl<CR> CodecReader for MergingCodecReader<CR>
where
  CR: CodecReader,
{
  type StoredFieldsReader = CR::StoredFieldsReader;
  type TermVectorsReader = CR::TermVectorsReader;
  type NormsProducer = CR::NormsProducer;
  type DocValuesProducer = CR::DocValuesProducer;
  type FieldsProducer = CR::FieldsProducer;
  type PointsReader = CR::PointsReader;
  type KnnVectorsReader = CR::KnnVectorsReader;

  fn get_fields_reader(&self) -> Result<Option<Self::StoredFieldsReader>> {
    match self.in_.get_fields_reader()? {
      Some(reader) => {
        let merge_instance = reader.get_merge_instance()?;
        Ok(Some(merge_instance.unwrap_or(reader)))
      },
      None => Ok(None),
    }
  }

  fn get_term_vectors_reader(&self) -> Result<Option<Self::TermVectorsReader>> {
    self.in_.get_term_vectors_reader()
  }

  fn get_norms_reader(&self) -> Result<Option<Self::NormsProducer>> {
    match self.in_.get_norms_reader()? {
      Some(reader) => {
        let merge_instance = reader.get_merge_instance()?;
        Ok(Some(merge_instance.unwrap_or(reader)))
      },
      None => Ok(None),
    }
  }

  fn get_doc_values_reader(&self) -> Result<Option<Self::DocValuesProducer>> {
    match self.in_.get_doc_values_reader()? {
      Some(reader) => {
        let merge_instance = reader.get_merge_instance()?;
        Ok(Some(merge_instance.unwrap_or(reader)))
      },
      None => Ok(None),
    }
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

/// Either an unchanged codec reader or the same reader using merge instances.
///
/// Both variants expose the same associated types, so using a generic
/// two-type codec reader enum here would only wrap every associated value in
/// another enum.
pub enum MergingCodecReaderEnum<CR>
where
  CR: CodecReader,
{
  Default(CR),
  Merging(MergingCodecReader<CR>),
}

impl<CR> Clone for MergingCodecReaderEnum<CR>
where
  CR: CodecReader + Clone,
{
  fn clone(&self) -> Self {
    match self {
      Self::Default(reader) => Self::Default(reader.clone()),
      Self::Merging(reader) => Self::Merging(reader.clone()),
    }
  }
}

impl<CR> Display for MergingCodecReaderEnum<CR>
where
  CR: CodecReader,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Default(reader) => Display::fmt(reader, f),
      Self::Merging(reader) => Display::fmt(reader, f),
    }
  }
}

impl<CR> IndexReader for MergingCodecReaderEnum<CR>
where
  CR: CodecReader,
{
  type ContextKind = LeafReaderContextKind;
  type TermVectors = TermVectorsType<CR::TermVectorsReader>;

  fn term_vectors(&self) -> Result<Self::TermVectors> {
    CodecReader::term_vectors(self)
  }

  fn max_doc(&self) -> Result<i32> {
    match self {
      Self::Default(reader) => reader.max_doc(),
      Self::Merging(reader) => reader.max_doc(),
    }
  }

  fn num_docs(&self) -> Result<i32> {
    match self {
      Self::Default(reader) => reader.num_docs(),
      Self::Merging(reader) => reader.num_docs(),
    }
  }

  type StoredFields = StoredFieldsType<CR::StoredFieldsReader>;

  fn stored_fields(&self) -> Result<Self::StoredFields> {
    CodecReader::stored_fields(self)
  }

  type ReaderCacheHelper = CR::ReaderCacheHelper;

  fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
    match self {
      Self::Default(reader) => reader.get_reader_cache_helper(),
      Self::Merging(reader) => reader.get_reader_cache_helper(),
    }
  }

  fn doc_freq(&self, term: &Term) -> Result<i32> {
    match self {
      Self::Default(reader) => IndexReader::doc_freq(reader, term),
      Self::Merging(reader) => IndexReader::doc_freq(reader, term),
    }
  }

  fn total_term_freq(&self, term: &Term) -> Result<i64> {
    match self {
      Self::Default(reader) => IndexReader::total_term_freq(reader, term),
      Self::Merging(reader) => IndexReader::total_term_freq(reader, term),
    }
  }

  fn get_sum_doc_freq(&self, field: &str) -> Result<i64> {
    match self {
      Self::Default(reader) => IndexReader::get_sum_doc_freq(reader, field),
      Self::Merging(reader) => IndexReader::get_sum_doc_freq(reader, field),
    }
  }

  fn get_doc_count(&self, field: &str) -> Result<i32> {
    match self {
      Self::Default(reader) => IndexReader::get_doc_count(reader, field),
      Self::Merging(reader) => IndexReader::get_doc_count(reader, field),
    }
  }

  fn get_sum_total_term_freq(&self, field: &str) -> Result<i64> {
    match self {
      Self::Default(reader) => IndexReader::get_sum_total_term_freq(reader, field),
      Self::Merging(reader) => IndexReader::get_sum_total_term_freq(reader, field),
    }
  }

  fn index_base(&self) -> &IndexReaderBase {
    match self {
      Self::Default(reader) => reader.index_base(),
      Self::Merging(reader) => reader.index_base(),
    }
  }
}

impl<CR> LeafReader for MergingCodecReaderEnum<CR>
where
  CR: CodecReader,
{
  type CacheHelper = CR::CacheHelper;

  fn get_core_cache_helper(&self) -> Result<Option<Self::CacheHelper>> {
    match self {
      Self::Default(reader) => reader.get_core_cache_helper(),
      Self::Merging(reader) => reader.get_core_cache_helper(),
    }
  }

  type Terms = CR::Terms;

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    match self {
      Self::Default(reader) => LeafReader::terms(reader, field),
      Self::Merging(reader) => LeafReader::terms(reader, field),
    }
  }

  type NumericDocValues = <CR::DocValuesProducer as DocValuesProducer>::NumericDocValues;

  fn get_numeric_doc_values(&self, field: &str) -> Result<Option<Self::NumericDocValues>> {
    match self {
      Self::Default(reader) => CodecReader::get_numeric_doc_values(reader, field),
      Self::Merging(reader) => CodecReader::get_numeric_doc_values(reader, field),
    }
  }

  type BinaryDocValues = <CR::DocValuesProducer as DocValuesProducer>::BinaryDocValues;

  fn get_binary_doc_values(&self, field: &str) -> Result<Option<Self::BinaryDocValues>> {
    match self {
      Self::Default(reader) => CodecReader::get_binary_doc_values(reader, field),
      Self::Merging(reader) => CodecReader::get_binary_doc_values(reader, field),
    }
  }

  type SortedDocValues = <CR::DocValuesProducer as DocValuesProducer>::SortedDocValues;

  fn get_sorted_doc_values(&self, field: &str) -> Result<Option<Self::SortedDocValues>> {
    match self {
      Self::Default(reader) => CodecReader::get_sorted_doc_values(reader, field),
      Self::Merging(reader) => CodecReader::get_sorted_doc_values(reader, field),
    }
  }

  type SortedNumericDocValues =
    <CR::DocValuesProducer as DocValuesProducer>::SortedNumericDocValues;

  fn get_sorted_numeric_doc_values(
    &self,
    field: &str,
  ) -> Result<Option<Self::SortedNumericDocValues>> {
    match self {
      Self::Default(reader) => CodecReader::get_sorted_numeric_doc_values(reader, field),
      Self::Merging(reader) => CodecReader::get_sorted_numeric_doc_values(reader, field),
    }
  }

  type SortedSetDocValues = <CR::DocValuesProducer as DocValuesProducer>::SortedSetDocValues;

  fn get_sorted_set_doc_values(&self, field: &str) -> Result<Option<Self::SortedSetDocValues>> {
    match self {
      Self::Default(reader) => CodecReader::get_sorted_set_doc_values(reader, field),
      Self::Merging(reader) => CodecReader::get_sorted_set_doc_values(reader, field),
    }
  }

  type NormNumericDocValues = <CR::NormsProducer as NormsProducer>::NumericDocValues;

  fn get_norm_values(&self, field: &str) -> Result<Option<Self::NormNumericDocValues>> {
    match self {
      Self::Default(reader) => CodecReader::get_norm_values(reader, field),
      Self::Merging(reader) => CodecReader::get_norm_values(reader, field),
    }
  }

  type DocValuesSkipper = <CR::DocValuesProducer as DocValuesProducer>::DocValuesSkipper;

  fn get_doc_values_skipper(&self, field: &str) -> Result<Option<Self::DocValuesSkipper>> {
    match self {
      Self::Default(reader) => CodecReader::get_doc_values_skipper(reader, field),
      Self::Merging(reader) => CodecReader::get_doc_values_skipper(reader, field),
    }
  }

  type FloatVectorValues = CR::FloatVectorValues;

  fn get_float_vector_values(&self, field: &str) -> Result<Option<Self::FloatVectorValues>> {
    match self {
      Self::Default(reader) => LeafReader::get_float_vector_values(reader, field),
      Self::Merging(reader) => LeafReader::get_float_vector_values(reader, field),
    }
  }

  type ByteVectorValues = CR::ByteVectorValues;

  fn get_byte_vector_values(&self, field: &str) -> Result<Option<Self::ByteVectorValues>> {
    match self {
      Self::Default(reader) => LeafReader::get_byte_vector_values(reader, field),
      Self::Merging(reader) => LeafReader::get_byte_vector_values(reader, field),
    }
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
    match self {
      Self::Default(reader) => {
        LeafReader::search_nearest_vectors_f32(reader, field, target, knn_collector, accept_docs)
      },
      Self::Merging(reader) => {
        LeafReader::search_nearest_vectors_f32(reader, field, target, knn_collector, accept_docs)
      },
    }
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
    match self {
      Self::Default(reader) => {
        LeafReader::search_nearest_vectors_u8(reader, field, target, knn_collector, accept_docs)
      },
      Self::Merging(reader) => {
        LeafReader::search_nearest_vectors_u8(reader, field, target, knn_collector, accept_docs)
      },
    }
  }

  fn get_field_infos(&self) -> Result<Arc<FieldInfos>> {
    match self {
      Self::Default(reader) => reader.get_field_infos(),
      Self::Merging(reader) => reader.get_field_infos(),
    }
  }

  type Bits = CR::Bits;

  fn get_live_docs(&self) -> Result<Option<Self::Bits>> {
    match self {
      Self::Default(reader) => reader.get_live_docs(),
      Self::Merging(reader) => reader.get_live_docs(),
    }
  }

  type PointValues = CR::PointValues;

  fn get_point_values(&self, field: &str) -> Result<Option<Self::PointValues>> {
    match self {
      Self::Default(reader) => LeafReader::get_point_values(reader, field),
      Self::Merging(reader) => LeafReader::get_point_values(reader, field),
    }
  }

  fn get_metadata(&self) -> Result<&LeafMetaData> {
    match self {
      Self::Default(reader) => reader.get_metadata(),
      Self::Merging(reader) => reader.get_metadata(),
    }
  }
}

impl<CR> CodecReader for MergingCodecReaderEnum<CR>
where
  CR: CodecReader,
{
  type StoredFieldsReader = CR::StoredFieldsReader;
  type TermVectorsReader = CR::TermVectorsReader;
  type NormsProducer = CR::NormsProducer;
  type DocValuesProducer = CR::DocValuesProducer;
  type FieldsProducer = CR::FieldsProducer;
  type PointsReader = CR::PointsReader;
  type KnnVectorsReader = CR::KnnVectorsReader;

  fn get_fields_reader(&self) -> Result<Option<Self::StoredFieldsReader>> {
    match self {
      Self::Default(reader) => reader.get_fields_reader(),
      Self::Merging(reader) => reader.get_fields_reader(),
    }
  }

  fn get_term_vectors_reader(&self) -> Result<Option<Self::TermVectorsReader>> {
    match self {
      Self::Default(reader) => reader.get_term_vectors_reader(),
      Self::Merging(reader) => reader.get_term_vectors_reader(),
    }
  }

  fn get_norms_reader(&self) -> Result<Option<Self::NormsProducer>> {
    match self {
      Self::Default(reader) => reader.get_norms_reader(),
      Self::Merging(reader) => reader.get_norms_reader(),
    }
  }

  fn get_doc_values_reader(&self) -> Result<Option<Self::DocValuesProducer>> {
    match self {
      Self::Default(reader) => reader.get_doc_values_reader(),
      Self::Merging(reader) => reader.get_doc_values_reader(),
    }
  }

  fn get_postings_reader(&self) -> Result<Option<Self::FieldsProducer>> {
    match self {
      Self::Default(reader) => reader.get_postings_reader(),
      Self::Merging(reader) => reader.get_postings_reader(),
    }
  }

  fn get_points_reader(&self) -> Result<Option<Self::PointsReader>> {
    match self {
      Self::Default(reader) => reader.get_points_reader(),
      Self::Merging(reader) => reader.get_points_reader(),
    }
  }

  fn get_vector_reader(&self) -> Result<Option<Self::KnnVectorsReader>> {
    match self {
      Self::Default(reader) => reader.get_vector_reader(),
      Self::Merging(reader) => reader.get_vector_reader(),
    }
  }
}
