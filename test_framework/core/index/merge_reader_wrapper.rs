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
use std::fmt::{Display, Formatter};
use std::sync::Arc;

use crate::core::codecs::doc_values_producer::DocValuesProducer;
use crate::core::codecs::fields_producer::FieldsProducer;
use crate::core::codecs::norms_producer::NormsProducer;
use crate::core::codecs::stored_fields_reader::StoredFieldsReader;
use crate::core::codecs::stored_fields_writer::StoredFieldsWriter;
use crate::core::codecs::term_vectors_reader::{DefaultTermVectorsReader, TermVectorsReader};
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::fields::Fields;
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
use crate::core::util::error::lucene_error::{LuceneError, Result};
use parking_lot::Mutex;

pub(crate) enum MergeReaderWrapperTermVectors<TVR> {
  Empty,
  Reader(TVR),
}

impl<TVR> RawTermVectors for MergeReaderWrapperTermVectors<TVR>
where
  TVR: RawTermVectors,
{
  type IndexInput = TVR::IndexInput;

  fn raw_term_vectors_mut(&mut self) -> Result<&mut DefaultTermVectorsReader<Self::IndexInput>> {
    match self {
      Self::Empty => Err(LuceneError::illegal_state(
        "raw term vectors reader is not available",
      )),
      Self::Reader(reader) => reader.raw_term_vectors_mut(),
    }
  }

  fn raw_term_vectors(&self) -> Result<&DefaultTermVectorsReader<Self::IndexInput>> {
    match self {
      Self::Empty => Err(LuceneError::illegal_state(
        "raw term vectors reader is not available",
      )),
      Self::Reader(reader) => reader.raw_term_vectors(),
    }
  }
}

impl<TVR> TermVectors for MergeReaderWrapperTermVectors<TVR>
where
  TVR: TermVectors,
{
  fn prefetch(&mut self, doc_id: i32) -> Result<()> {
    match self {
      Self::Empty => Ok(()),
      Self::Reader(reader) => reader.prefetch(doc_id),
    }
  }

  type Fields = TVR::Fields;

  fn get(&mut self, doc: i32) -> Result<Option<Self::Fields>> {
    match self {
      Self::Empty => Ok(None),
      Self::Reader(reader) => reader.get(doc),
    }
  }

  type Terms = TVR::Terms;

  fn get_field_terms(
    &mut self,
    doc: i32,
    field: &str,
  ) -> Result<Option<<Self::Fields as Fields>::Terms>> {
    match self {
      Self::Empty => Ok(None),
      Self::Reader(reader) => reader.get_field_terms(doc, field),
    }
  }
}

/// Adapts Java `MergeReaderWrapper`'s shared `StoredFieldsReader` reference to Rust's owned
/// [`IndexReader::StoredFields`] return type.
///
/// Java returns the same merge instance from `storedFields()`. Returning the concrete reader from
/// Rust would require cloning it, but stored-fields merge instances must not be cloned. The `Arc`
/// keeps every returned handle attached to the same reader, while the `Mutex` provides the mutable
/// access required by [`StoredFields`].
pub(crate) struct MergeReaderWrapperStoredFields<SFR> {
  in_: Arc<Mutex<SFR>>,
}

impl<SFR> MergeReaderWrapperStoredFields<SFR>
where
  SFR: StoredFieldsReader,
{
  fn new(in_: SFR) -> Self {
    Self {
      in_: Arc::new(Mutex::new(in_)),
    }
  }
}

impl<SFR> Clone for MergeReaderWrapperStoredFields<SFR>
where
  SFR: StoredFieldsReader,
{
  fn clone(&self) -> Self {
    Self {
      in_: self.in_.clone(),
    }
  }
}

impl<SFR> RawStoredFieldsReader for MergeReaderWrapperStoredFields<SFR>
where
  SFR: StoredFieldsReader,
{
  type IndexInput = SFR::IndexInput;
}

impl<SFR> StoredFields for MergeReaderWrapperStoredFields<SFR>
where
  SFR: StoredFieldsReader,
{
  fn prefetch(&mut self, doc_id: i32) -> Result<()> {
    self.in_.lock().prefetch(doc_id)
  }

  fn document_with_visitor<W>(
    &mut self,
    doc_id: i32,
    visitor: &mut impl StoredFieldVisitor,
    writer: Option<&mut W>,
  ) -> Result<()>
  where
    W: StoredFieldsWriter,
  {
    self
      .in_
      .lock()
      .document_with_visitor(doc_id, visitor, writer)
  }
}

/// This is a hack to make index sorting fast, with a [`LeafReader`] that always returns merge
/// instances when you ask for the codec readers.
pub(crate) struct MergeReaderWrapper<CR>
where
  CR: CodecReader,
{
  in_: CR,
  fields: Option<CR::FieldsProducer>,
  norms: Option<CR::NormsProducer>,
  doc_values: Option<CR::DocValuesProducer>,
  store: Option<MergeReaderWrapperStoredFields<CR::StoredFieldsReader>>,
  vectors: Option<CR::TermVectorsReader>,
  index_base: IndexReaderBase,
}

impl<CR> MergeReaderWrapper<CR>
where
  CR: CodecReader,
{
  pub(crate) fn new(in_: CR) -> Result<Self> {
    let fields = match in_.get_postings_reader()? {
      Some(fields) => {
        let merge_instance = fields.get_merge_instance()?;
        Some(merge_instance.unwrap_or(fields))
      },
      None => None,
    };

    let norms = match in_.get_norms_reader()? {
      Some(norms) => {
        let merge_instance = norms.get_merge_instance()?;
        Some(merge_instance.unwrap_or(norms))
      },
      None => None,
    };

    let doc_values = match in_.get_doc_values_reader()? {
      Some(doc_values) => {
        let merge_instance = doc_values.get_merge_instance()?;
        Some(merge_instance.unwrap_or(doc_values))
      },
      None => None,
    };

    let store = match in_.get_fields_reader()? {
      Some(store) => {
        let merge_instance = store.get_merge_instance()?;
        Some(MergeReaderWrapperStoredFields::new(
          merge_instance.unwrap_or(store),
        ))
      },
      None => None,
    };

    let vectors = match in_.get_term_vectors_reader()? {
      Some(vectors) => {
        let merge_instance = vectors.get_merge_instance()?;
        Some(merge_instance.unwrap_or(vectors))
      },
      None => None,
    };

    Ok(Self {
      in_,
      fields,
      norms,
      doc_values,
      store,
      vectors,
      index_base: IndexReaderBase::new(),
    })
  }
}

impl<CR> LeafReader for MergeReaderWrapper<CR>
where
  CR: CodecReader,
{
  type CacheHelper = CR::CacheHelper;

  fn get_core_cache_helper(&self) -> Result<Option<Self::CacheHelper>> {
    self.in_.get_core_cache_helper()
  }

  type Terms = <CR::FieldsProducer as Fields>::Terms;

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    self.ensure_open()?;
    // We could check the FieldInfo IndexOptions but there's no point since
    // PostingsReader will simply return None for fields that don't exist or that have no terms
    // index.
    match &self.fields {
      Some(fields) => fields.terms(field),
      None => Ok(None),
    }
  }

  type NumericDocValues = <CR::DocValuesProducer as DocValuesProducer>::NumericDocValues;

  fn get_numeric_doc_values(&self, field: &str) -> Result<Option<Self::NumericDocValues>> {
    self.ensure_open()?;
    let fi = self.get_field_infos()?.field_info_by_name(field)?;
    let fi = match fi {
      Some(fi) => fi,
      None => return Ok(None), // Field does not exist
    };
    if *fi.get_doc_values_type() != DocValuesType::Numeric {
      // Field was not indexed with doc values
      return Ok(None);
    }
    let doc_values = self
      .doc_values
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("doc values reader is None"))?;
    Ok(Some(doc_values.get_numeric(&fi)?))
  }

  type BinaryDocValues = <CR::DocValuesProducer as DocValuesProducer>::BinaryDocValues;

  fn get_binary_doc_values(&self, field: &str) -> Result<Option<Self::BinaryDocValues>> {
    self.ensure_open()?;
    let fi = self.get_field_infos()?.field_info_by_name(field)?;
    let fi = match fi {
      Some(fi) => fi,
      None => return Ok(None), // Field does not exist
    };
    if *fi.get_doc_values_type() != DocValuesType::Binary {
      // Field was not indexed with doc values
      return Ok(None);
    }
    let doc_values = self
      .doc_values
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("doc values reader is None"))?;
    Ok(Some(doc_values.get_binary(&fi)?))
  }

  type SortedDocValues = <CR::DocValuesProducer as DocValuesProducer>::SortedDocValues;

  fn get_sorted_doc_values(&self, field: &str) -> Result<Option<Self::SortedDocValues>> {
    self.ensure_open()?;
    let fi = self.get_field_infos()?.field_info_by_name(field)?;
    let fi = match fi {
      Some(fi) => fi,
      None => return Ok(None), // Field does not exist
    };
    if *fi.get_doc_values_type() != DocValuesType::Sorted {
      // Field was not indexed with doc values
      return Ok(None);
    }
    let doc_values = self
      .doc_values
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("doc values reader is None"))?;
    Ok(Some(doc_values.get_sorted(&fi)?))
  }

  type SortedNumericDocValues =
    <CR::DocValuesProducer as DocValuesProducer>::SortedNumericDocValues;

  fn get_sorted_numeric_doc_values(
    &self,
    field: &str,
  ) -> Result<Option<Self::SortedNumericDocValues>> {
    self.ensure_open()?;
    let fi = self.get_field_infos()?.field_info_by_name(field)?;
    let fi = match fi {
      Some(fi) => fi,
      None => return Ok(None), // Field does not exist
    };
    if *fi.get_doc_values_type() != DocValuesType::SortedNumeric {
      // Field was not indexed with doc values
      return Ok(None);
    }
    let doc_values = self
      .doc_values
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("doc values reader is None"))?;
    Ok(Some(doc_values.get_sorted_numeric(&fi)?))
  }

  type SortedSetDocValues = <CR::DocValuesProducer as DocValuesProducer>::SortedSetDocValues;

  fn get_sorted_set_doc_values(&self, field: &str) -> Result<Option<Self::SortedSetDocValues>> {
    self.ensure_open()?;
    let fi = self.get_field_infos()?.field_info_by_name(field)?;
    let fi = match fi {
      Some(fi) => fi,
      None => return Ok(None), // Field does not exist
    };
    if *fi.get_doc_values_type() != DocValuesType::SortedSet {
      // Field was not indexed with doc values
      return Ok(None);
    }
    let doc_values = self
      .doc_values
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("doc values reader is None"))?;
    Ok(Some(doc_values.get_sorted_set(&fi)?))
  }

  type NormNumericDocValues = <CR::NormsProducer as NormsProducer>::NumericDocValues;

  fn get_norm_values(&self, field: &str) -> Result<Option<Self::NormNumericDocValues>> {
    self.ensure_open()?;
    let fi = self.get_field_infos()?.field_info_by_name(field)?;
    let fi = match fi {
      Some(fi) if fi.has_norms() => fi,
      _ => return Ok(None), // Field does not exist or does not index norms
    };
    let norms = self
      .norms
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("norms reader is None"))?;
    Ok(Some(norms.get_norms(&fi)?))
  }

  type DocValuesSkipper = <CR::DocValuesProducer as DocValuesProducer>::DocValuesSkipper;

  fn get_doc_values_skipper(&self, field: &str) -> Result<Option<Self::DocValuesSkipper>> {
    self.ensure_open()?;
    let fi = self.get_field_infos()?.field_info_by_name(field)?;
    let fi = match fi {
      Some(fi) => fi,
      None => return Ok(None), // Field does not exist
    };
    let doc_values = self
      .doc_values
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("doc values reader is None"))?;
    doc_values.get_skipper(&fi)
  }

  fn get_field_infos(&self) -> Result<Arc<FieldInfos>> {
    self.in_.get_field_infos()
  }

  type Bits = CR::Bits;

  fn get_live_docs(&self) -> Result<Option<Self::Bits>> {
    self.in_.get_live_docs()
  }

  fn check_integrity(&self) -> Result<()> {
    self.in_.check_integrity()
  }

  type PointValues = CR::PointValues;

  fn get_point_values(&self, field_name: &str) -> Result<Option<Self::PointValues>> {
    LeafReader::get_point_values(&self.in_, field_name)
  }

  type FloatVectorValues = CR::FloatVectorValues;

  fn get_float_vector_values(&self, field_name: &str) -> Result<Option<Self::FloatVectorValues>> {
    LeafReader::get_float_vector_values(&self.in_, field_name)
  }

  type ByteVectorValues = CR::ByteVectorValues;

  fn get_byte_vector_values(&self, field_name: &str) -> Result<Option<Self::ByteVectorValues>> {
    LeafReader::get_byte_vector_values(&self.in_, field_name)
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

  fn get_metadata(&self) -> Result<&LeafMetaData> {
    self.in_.get_metadata()
  }
}

impl<CR> IndexReader for MergeReaderWrapper<CR>
where
  CR: CodecReader,
{
  type ContextKind = LeafReaderContextKind;

  type TermVectors = MergeReaderWrapperTermVectors<CR::TermVectorsReader>;

  fn term_vectors(&self) -> Result<Self::TermVectors> {
    self.ensure_open()?;
    match &self.vectors {
      Some(vectors) => Ok(MergeReaderWrapperTermVectors::Reader(vectors.try_clone()?)),
      None => Ok(MergeReaderWrapperTermVectors::Empty),
    }
  }

  fn num_docs(&self) -> Result<i32> {
    self.in_.num_docs()
  }

  fn max_doc(&self) -> Result<i32> {
    self.in_.max_doc()
  }

  type StoredFields = MergeReaderWrapperStoredFields<CR::StoredFieldsReader>;

  fn stored_fields(&self) -> Result<Self::StoredFields> {
    self.ensure_open()?;
    self
      .store
      .as_ref()
      .cloned()
      .ok_or_else(|| LuceneError::illegal_state("stored fields reader is None"))
  }

  fn do_close(&self) -> Result<()> {
    self.in_.close()
  }

  type ReaderCacheHelper = CR::ReaderCacheHelper;

  fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
    self.in_.get_reader_cache_helper()
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

impl<CR> Display for MergeReaderWrapper<CR>
where
  CR: CodecReader,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "MergeReaderWrapper({})", self.in_)
  }
}
