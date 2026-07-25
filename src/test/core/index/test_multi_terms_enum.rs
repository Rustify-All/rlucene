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
use crate::core::codecs::knn_vectors_reader::KnnVectorsReader;
use crate::core::codecs::norms_producer::NormsProducer;
use crate::core::codecs::stored_fields_reader::StoredFieldsReader;
use crate::core::codecs::stored_fields_writer::StoredFieldsWriter;
use crate::core::document::document::Document;
use crate::core::document::field::Store;
use crate::core::document::string_field::StringField;
use crate::core::index::BytesRef;
use crate::core::index::codec_reader::{CodecReader, TermVectorsType};
use crate::core::index::directory_reader;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::fields::Fields;
use crate::core::index::filtered_terms_enum::{
  AcceptStatus, FilteredTermsEnum, FilteredTermsEnumBase,
};
use crate::core::index::index_reader::{IndexReader, IndexReaderBase, LeafReaderContextKind};
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::leaf_metadata::LeafMetaData;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::stored_field_visitor::StoredFieldVisitor;
use crate::core::index::stored_fields::{RawStoredFieldsReader, StoredFields};
use crate::core::index::term::Term;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::{SeekStatus, TermsEnum};
use crate::core::search::knn_collector::KnnCollector;
use crate::core::util::ToInt;
use crate::core::util::bits::Bits;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::clone::TryClone;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::iterator::IteratorExt;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::util::lucene_test_case::{
  new_directory_shared, new_index_writer_config_with_analyzer, random,
};
use std::borrow::Cow;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

#[allow(dead_code)] // for quick search
struct TestMultiTermsEnum;

// LUCENE-6826
#[test]
fn test_no_terms_in_field() -> Result<()> {
  let mut random = random();
  let directory = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let writer = IndexWriter::new(directory.clone(), IndexWriterConfig::with_analyzer(a)?)?;
  let mut document = Document::new();
  document.add(StringField::from_string("deleted", "0", Store::Yes)?);
  writer.add_document(document)?;

  let reader = directory_reader::open_from_writer(&writer)?;
  writer.close()?;

  let directory2 = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let writer = IndexWriter::new(directory2.clone(), IndexWriterConfig::with_analyzer(a)?)?;

  let irc = (&reader).get_context()?;
  let leaves = irc.leaves()?;
  let mut codec_readers = Vec::with_capacity(leaves.len());
  for leaf in leaves {
    let codec_reader = leaf.reader().clone();
    codec_readers.push(MigratingCodecReader::new(
      codec_reader,
      leaf.reader().get_field_infos()?,
    )?);
  }
  writer.add_indexes_from_codec_readers(codec_readers)?;

  writer.close()?;
  reader.close()?;
  directory.close()?;

  Ok(())
}

/// Wraps a CodecReader, delegating all calls except get_postings_reader which
/// returns a MigratingFieldsProducer that can filter terms for specific fields.
pub struct MigratingCodecReader<CR>
where
  CR: CodecReader,
{
  in_: CR,
  field_infos: Arc<FieldInfos>,
}

impl<CR> MigratingCodecReader<CR>
where
  CR: CodecReader,
{
  pub fn new(in_: CR, field_infos: Arc<FieldInfos>) -> Result<Self> {
    Ok(Self { in_, field_infos })
  }
}

impl<CR> Display for MigratingCodecReader<CR>
where
  CR: CodecReader,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "MigratingCodecReader({})", self.in_)
  }
}

impl<CR> IndexReader for MigratingCodecReader<CR>
where
  CR: CodecReader,
  CR::StoredFieldsReader: RawStoredFieldsReader,
{
  type ContextKind = LeafReaderContextKind;

  type TermVectors = TermVectorsType<CR::TermVectorsReader>;

  fn term_vectors(&self) -> Result<Self::TermVectors> {
    CodecReader::term_vectors(&self.in_)
  }

  fn max_doc(&self) -> Result<i32> {
    self.in_.max_doc()
  }

  fn num_docs(&self) -> Result<i32> {
    self.in_.num_docs()
  }

  type StoredFields = crate::core::index::codec_reader::StoredFieldsType<
    MigratingStoredFieldsReader<CR::StoredFieldsReader>,
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
    LeafReader::doc_freq(&self.in_, term)
  }

  fn total_term_freq(&self, term: &Term) -> Result<i64> {
    self.in_.total_term_freq(term)
  }

  fn get_sum_doc_freq(&self, field: &str) -> Result<i64> {
    LeafReader::get_sum_doc_freq(&self.in_, field)
  }

  fn get_doc_count(&self, field: &str) -> Result<i32> {
    LeafReader::get_doc_count(&self.in_, field)
  }

  fn get_sum_total_term_freq(&self, field: &str) -> Result<i64> {
    LeafReader::get_sum_total_term_freq(&self.in_, field)
  }

  fn index_base(&self) -> &IndexReaderBase {
    self.in_.index_base()
  }
}

impl<CR> LeafReader for MigratingCodecReader<CR>
where
  CR: CodecReader,
  CR::StoredFieldsReader: RawStoredFieldsReader,
{
  type CacheHelper = CR::CacheHelper;

  fn get_core_cache_helper(&self) -> Result<Option<Self::CacheHelper>> {
    self.in_.get_core_cache_helper()
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
    CodecReader::search_nearest_vectors_u8(&self.in_, field, target, knn_collector, accept_docs)
  }

  fn get_field_infos(&self) -> Result<Arc<FieldInfos>> {
    Ok(self.field_infos.clone())
  }

  type Bits = CR::Bits;

  fn get_live_docs(&self) -> Result<Option<Self::Bits>> {
    self.in_.get_live_docs()
  }

  type PointValues = CR::PointValues;

  fn get_point_values(&self, field: &str) -> Result<Option<Self::PointValues>> {
    LeafReader::get_point_values(&self.in_, field)
  }

  fn get_metadata(&self) -> Result<&LeafMetaData> {
    self.in_.get_metadata()
  }
}

impl<CR> CodecReader for MigratingCodecReader<CR>
where
  CR: CodecReader,
  CR::StoredFieldsReader: RawStoredFieldsReader,
{
  type StoredFieldsReader = MigratingStoredFieldsReader<CR::StoredFieldsReader>;
  type TermVectorsReader = CR::TermVectorsReader;
  type NormsProducer = CR::NormsProducer;
  type DocValuesProducer = CR::DocValuesProducer;
  type FieldsProducer = MigratingFieldsProducer<CR::FieldsProducer>;
  type PointsReader = CR::PointsReader;
  type KnnVectorsReader = CR::KnnVectorsReader;

  fn get_fields_reader(&self) -> Result<Option<Self::StoredFieldsReader>> {
    self
      .in_
      .get_fields_reader()
      .map(|opt| opt.map(|r| MigratingStoredFieldsReader::new(r, self.field_infos.clone())))
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
    self
      .in_
      .get_postings_reader()
      .map(|opt| opt.map(|fp| MigratingFieldsProducer::new(fp, self.field_infos.clone())))
  }

  fn get_points_reader(&self) -> Result<Option<Self::PointsReader>> {
    self.in_.get_points_reader()
  }

  fn get_vector_reader(&self) -> Result<Option<Self::KnnVectorsReader>> {
    self.in_.get_vector_reader()
  }
}

// ---- MigratingStoredFieldsReader ----
pub struct MigratingStoredFieldsReader<SFR>
where
  SFR: StoredFieldsReader + RawStoredFieldsReader,
{
  in_: SFR,
  field_infos: Arc<FieldInfos>,
}

impl<SFR> MigratingStoredFieldsReader<SFR>
where
  SFR: StoredFieldsReader + RawStoredFieldsReader,
{
  fn new(in_: SFR, field_infos: Arc<FieldInfos>) -> Self {
    Self { in_, field_infos }
  }
}

impl<SFR> TryClone for MigratingStoredFieldsReader<SFR>
where
  SFR: StoredFieldsReader + RawStoredFieldsReader,
{
  fn try_clone(&self) -> Result<Self>
  where
    Self: Sized,
  {
    Ok(Self::new(self.in_.try_clone()?, self.field_infos.clone()))
  }
}

impl<SFR> RawStoredFieldsReader for MigratingStoredFieldsReader<SFR>
where
  SFR: StoredFieldsReader + RawStoredFieldsReader,
{
  type IndexInput = SFR::IndexInput;

  fn raw_stored_fields_mut(
    &mut self,
  ) -> Result<
    &mut crate::core::codecs::stored_fields_reader::DefaultStoredFieldsReader<Self::IndexInput>,
  > {
    self.in_.raw_stored_fields_mut()
  }

  fn raw_stored_fields(
    &self,
  ) -> Result<&crate::core::codecs::stored_fields_reader::DefaultStoredFieldsReader<Self::IndexInput>>
  {
    self.in_.raw_stored_fields()
  }
}

impl<SFR> StoredFields for MigratingStoredFieldsReader<SFR>
where
  SFR: StoredFieldsReader + RawStoredFieldsReader,
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
    self.in_.document_with_visitor(doc_id, visitor, writer)
  }
}

impl<SFR> CloseableRef for MigratingStoredFieldsReader<SFR>
where
  SFR: StoredFieldsReader + RawStoredFieldsReader,
{
  fn close(&self) -> Result<()> {
    self.in_.close()
  }
}

impl<SFR> StoredFieldsReader for MigratingStoredFieldsReader<SFR>
where
  SFR: StoredFieldsReader + RawStoredFieldsReader,
{
  fn check_integrity(&self) -> Result<()> {
    self.in_.check_integrity()
  }

  fn get_merge_instance(&self) -> Result<Option<Self>> {
    match self.in_.get_merge_instance()? {
      Some(in_) => Ok(Some(Self::new(in_, self.field_infos.clone()))),
      None => Ok(None),
    }
  }
}

// ---- MigratingFieldsProducer ----

/// A FieldsProducer wrapper that uses `new_field_info` for field name iteration
/// and can optionally filter terms for specific fields.
pub struct MigratingFieldsProducer<FP>
where
  FP: FieldsProducer,
{
  delegate: FP,
  new_field_info: Arc<FieldInfos>,
}

impl<FP> MigratingFieldsProducer<FP>
where
  FP: FieldsProducer,
{
  fn new(delegate: FP, new_field_info: Arc<FieldInfos>) -> Self {
    Self {
      delegate,
      new_field_info,
    }
  }
}

impl<FP> CloseableRef for MigratingFieldsProducer<FP>
where
  FP: FieldsProducer,
{
  fn close(&self) -> Result<()> {
    self.delegate.close()
  }
}

/// Iterator over field names from FieldInfos.
pub struct FieldInfosIter<'a> {
  values: &'a [Arc<FieldInfo>],
  index: usize,
}

impl<'a> FieldInfosIter<'a> {
  fn new(field_infos: &'a FieldInfos) -> Self {
    FieldInfosIter {
      values: &field_infos.values,
      index: 0,
    }
  }
}

impl<'a> IteratorExt for FieldInfosIter<'a> {
  type Item = &'a String;

  fn next(&mut self) -> Result<Option<Self::Item>> {
    if self.index >= self.values.len() {
      return Ok(None);
    }
    let name = &self.values[self.index].name;
    self.index += 1;
    Ok(Some(name))
  }

  fn has_next(&self) -> Result<bool> {
    Ok(self.index < self.values.len())
  }
}

/// Enum for the Terms type returned by MigratingFieldsProducer.
pub enum MigratingTerms<T>
where
  T: Terms,
{
  Delegate(T),
  Filtered(ValueFilteredTerms<T>),
}

impl<T> Terms for MigratingTerms<T>
where
  T: Terms,
{
  type TermsEnum = MigratingTermsEnum<T::TermsEnum>;

  type IntersectIter = Self::TermsEnum;

  fn iterator(&self) -> Result<Self::TermsEnum> {
    match self {
      MigratingTerms::Delegate(t) => Ok(MigratingTermsEnum::Delegate(t.iterator()?)),
      MigratingTerms::Filtered(t) => Ok(MigratingTermsEnum::Filtered(t.iterator()?)),
    }
  }

  fn intersect(
    &self,
    _compiled: &crate::core::util::automation::compiled_automaton::CompiledAutomaton,
    _start_term: Option<&BytesRef<Vec<u8>>>,
  ) -> Result<Self::IntersectIter> {
    Err(LuceneError::unsupported_operation(
      "MigratingTerms::intersect",
    ))
  }

  fn size(&self) -> Result<i64> {
    match self {
      MigratingTerms::Delegate(t) => t.size(),
      MigratingTerms::Filtered(t) => t.size(),
    }
  }

  fn get_sum_total_term_freq(&self) -> Result<i64> {
    match self {
      MigratingTerms::Delegate(t) => t.get_sum_total_term_freq(),
      MigratingTerms::Filtered(t) => t.get_sum_total_term_freq(),
    }
  }

  fn get_sum_doc_freq(&self) -> Result<i64> {
    match self {
      MigratingTerms::Delegate(t) => t.get_sum_doc_freq(),
      MigratingTerms::Filtered(t) => t.get_sum_doc_freq(),
    }
  }

  fn get_doc_count(&self) -> Result<i32> {
    match self {
      MigratingTerms::Delegate(t) => t.get_doc_count(),
      MigratingTerms::Filtered(t) => t.get_doc_count(),
    }
  }

  fn has_freqs(&self) -> bool {
    match self {
      MigratingTerms::Delegate(t) => t.has_freqs(),
      MigratingTerms::Filtered(t) => t.has_freqs(),
    }
  }

  fn has_offsets(&self) -> bool {
    match self {
      MigratingTerms::Delegate(t) => t.has_offsets(),
      MigratingTerms::Filtered(t) => t.has_offsets(),
    }
  }

  fn has_positions(&self) -> bool {
    match self {
      MigratingTerms::Delegate(t) => t.has_positions(),
      MigratingTerms::Filtered(t) => t.has_positions(),
    }
  }

  fn has_payloads(&self) -> bool {
    match self {
      MigratingTerms::Delegate(t) => t.has_payloads(),
      MigratingTerms::Filtered(t) => t.has_payloads(),
    }
  }
}

/// Enum for the TermsEnum type returned by MigratingTerms.
pub enum MigratingTermsEnum<TE>
where
  TE: TermsEnum,
{
  Delegate(TE),
  Filtered(FilteredTermsEnum<TE, ValueFilteredTermsEnumBase>),
}

impl<TE> TermsEnum for MigratingTermsEnum<TE>
where
  TE: TermsEnum,
{
  type AttributeSource<'a>
    = TE::AttributeSource<'a>
  where
    Self: 'a;
  type AttributeSourceMut<'a>
    = TE::AttributeSourceMut<'a>
  where
    Self: 'a;

  fn attributes(&self) -> Result<Self::AttributeSource<'_>> {
    match self {
      MigratingTermsEnum::Delegate(e) => e.attributes(),
      MigratingTermsEnum::Filtered(e) => e.attributes(),
    }
  }

  fn attributes_mut(&mut self) -> Result<Self::AttributeSourceMut<'_>> {
    match self {
      MigratingTermsEnum::Delegate(e) => e.attributes_mut(),
      MigratingTermsEnum::Filtered(e) => e.attributes_mut(),
    }
  }

  fn seek_exact(&mut self, _term: &BytesRef<Vec<u8>>) -> Result<bool> {
    Err(LuceneError::unsupported_operation(
      "MigratingTermsEnum::seek_exact",
    ))
  }

  fn prepare_seek_exact(&mut self, _text: &BytesRef<Vec<u8>>) -> Result<Option<()>> {
    Err(LuceneError::unsupported_operation(
      "MigratingTermsEnum::prepare_seek_exact",
    ))
  }

  fn get_prepare_seek_exact_status(&mut self, _target: &BytesRef<Vec<u8>>) -> Result<bool> {
    Err(LuceneError::unsupported_operation(
      "MigratingTermsEnum::get_prepare_seek_exact_status",
    ))
  }

  fn seek_ceil(&mut self, _term: &BytesRef<Vec<u8>>) -> Result<SeekStatus> {
    Err(LuceneError::unsupported_operation(
      "MigratingTermsEnum::seek_ceil",
    ))
  }

  fn seek_exact_with_ord(&mut self, _ord: i64) -> Result<()> {
    Err(LuceneError::unsupported_operation(
      "MigratingTermsEnum::seek_exact_with_ord",
    ))
  }

  fn seek_exact_with_state(
    &mut self,
    _term: &BytesRef<Vec<u8>>,
    _state: &crate::core::codecs::block_term_state::TermStateEnum,
  ) -> Result<()> {
    Err(LuceneError::unsupported_operation(
      "MigratingTermsEnum::seek_exact_with_state",
    ))
  }

  fn term(&self) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
    match self {
      MigratingTermsEnum::Delegate(e) => e.term(),
      MigratingTermsEnum::Filtered(e) => e.term(),
    }
  }

  fn ord(&self) -> Result<i64> {
    match self {
      MigratingTermsEnum::Delegate(e) => e.ord(),
      MigratingTermsEnum::Filtered(e) => e.ord(),
    }
  }

  fn doc_freq(&mut self) -> Result<i32> {
    match self {
      MigratingTermsEnum::Delegate(e) => e.doc_freq(),
      MigratingTermsEnum::Filtered(e) => e.doc_freq(),
    }
  }

  fn total_term_freq(&mut self) -> Result<i64> {
    match self {
      MigratingTermsEnum::Delegate(e) => e.total_term_freq(),
      MigratingTermsEnum::Filtered(e) => e.total_term_freq(),
    }
  }

  type PostingsEnum = TE::PostingsEnum;

  fn postings_with_flags(
    &mut self,
    reuse: Option<Self::PostingsEnum>,
    flags: i32,
  ) -> Result<Self::PostingsEnum> {
    match self {
      MigratingTermsEnum::Delegate(e) => e.postings_with_flags(reuse, flags),
      MigratingTermsEnum::Filtered(e) => e.postings_with_flags(reuse, flags),
    }
  }

  type ImpactsEnum = TE::ImpactsEnum;

  fn impacts(&mut self, flags: i32) -> Result<Self::ImpactsEnum> {
    match self {
      MigratingTermsEnum::Delegate(e) => e.impacts(flags),
      MigratingTermsEnum::Filtered(e) => e.impacts(flags),
    }
  }

  fn term_state(&mut self) -> Result<crate::core::codecs::block_term_state::TermStateEnum> {
    match self {
      MigratingTermsEnum::Delegate(e) => e.term_state(),
      MigratingTermsEnum::Filtered(e) => e.term_state(),
    }
  }
}

impl<TE> BytesRefIterator for MigratingTermsEnum<TE>
where
  TE: TermsEnum + BytesRefIterator,
{
  fn next(&mut self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
    match self {
      MigratingTermsEnum::Delegate(e) => e.next(),
      MigratingTermsEnum::Filtered(e) => e.next(),
    }
  }
}

impl<FP> Fields for MigratingFieldsProducer<FP>
where
  FP: FieldsProducer,
{
  type FieldIter<'a>
    = FieldInfosIter<'a>
  where
    Self: 'a;

  type Terms = MigratingTerms<FP::Terms>;

  fn iterator(&self) -> Result<Self::FieldIter<'_>> {
    Ok(FieldInfosIter::new(&self.new_field_info))
  }

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    if field == "deleted" {
      let deleted_terms = self.delegate.terms("deleted")?;
      if let Some(t) = deleted_terms {
        return Ok(Some(MigratingTerms::Filtered(ValueFilteredTerms::new(
          t,
          BytesRef::from_string("1"),
        ))));
      }
      return Ok(None);
    }
    let terms = self.delegate.terms(field)?;
    Ok(terms.map(MigratingTerms::Delegate))
  }

  fn size(&self) -> Result<i32> {
    Ok(self.new_field_info.values.len() as i32)
  }
}

impl<FP> FieldsProducer for MigratingFieldsProducer<FP>
where
  FP: FieldsProducer,
{
  fn check_integrity(&self) -> Result<()> {
    self.delegate.check_integrity()
  }

  fn get_merge_instance(&self) -> Result<Option<Self>> {
    match self.delegate.get_merge_instance()? {
      Some(delegate) => Ok(Some(MigratingFieldsProducer::new(
        delegate,
        self.new_field_info.clone(),
      ))),
      None => Ok(None),
    }
  }
}

// ---- ValueFilteredTerms ----

/// Wraps a Terms and filters its iterator to only accept a specific value.
pub struct ValueFilteredTerms<T>
where
  T: Terms,
{
  delegate: T,
  value: BytesRef<Vec<u8>>,
}

impl<T> ValueFilteredTerms<T>
where
  T: Terms,
{
  pub fn new(delegate: T, value: BytesRef<Vec<u8>>) -> Self {
    Self { delegate, value }
  }
}

impl<T> Terms for ValueFilteredTerms<T>
where
  T: Terms,
{
  type TermsEnum = FilteredTermsEnum<T::TermsEnum, ValueFilteredTermsEnumBase>;

  type IntersectIter = Self::TermsEnum;

  fn iterator(&self) -> Result<Self::TermsEnum> {
    Ok(FilteredTermsEnum::with_seek(
      self.delegate.iterator()?,
      true,
      ValueFilteredTermsEnumBase {
        value: self.value.clone(),
      },
    ))
  }

  fn intersect(
    &self,
    _compiled: &crate::core::util::automation::compiled_automaton::CompiledAutomaton,
    _start_term: Option<&BytesRef<Vec<u8>>>,
  ) -> Result<Self::IntersectIter> {
    Err(LuceneError::unsupported_operation(
      "ValueFilteredTerms::intersect",
    ))
  }

  fn size(&self) -> Result<i64> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn get_sum_total_term_freq(&self) -> Result<i64> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn get_sum_doc_freq(&self) -> Result<i64> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn get_doc_count(&self) -> Result<i32> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn has_freqs(&self) -> bool {
    self.delegate.has_freqs()
  }

  fn has_offsets(&self) -> bool {
    self.delegate.has_offsets()
  }

  fn has_positions(&self) -> bool {
    self.delegate.has_positions()
  }

  fn has_payloads(&self) -> bool {
    self.delegate.has_payloads()
  }
}

// ---- ValueFilteredTermsEnumBase ----

/// FilteredTermsEnumBase implementation that only accepts a single matching term value.
pub struct ValueFilteredTermsEnumBase {
  value: BytesRef<Vec<u8>>,
}

impl FilteredTermsEnumBase for ValueFilteredTermsEnumBase {
  fn accept(&mut self, term: &BytesRef<Vec<u8>>, _ord: i64) -> Result<AcceptStatus> {
    let comparison = term.cmp(&self.value).to_int();
    if comparison < 0 {
      Ok(AcceptStatus::NoAndSeek)
    } else if comparison > 0 {
      Ok(AcceptStatus::End)
    } else {
      Ok(AcceptStatus::Yes)
    }
  }

  fn next_seek_term(
    &mut self,
    current_term: Option<&BytesRef<Vec<u8>>>,
  ) -> Result<Option<std::borrow::Cow<'_, BytesRef<Vec<u8>>>>> {
    if current_term.is_none() || current_term.unwrap().cmp(&self.value).to_int() < 0 {
      Ok(Some(std::borrow::Cow::Borrowed(&self.value)))
    } else {
      Ok(None)
    }
  }
}
