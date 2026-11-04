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
use crate::core::index::base_composite_reader::{
  BCRStoredFieldsImpl, BCRTermVectorsImpl, BaseCompositeReader, BaseCompositeReaderBase,
};
use crate::core::index::codec_reader::CodecReader;
pub use crate::core::index::codec_reader::SoftDeletesCodecReader;
use crate::core::index::composite_reader::CompositeReader;
use crate::core::index::directory_reader::{DirectoryReader, DirectoryReaderBase};
use crate::core::index::filter_directory_reader::{
  DelegatingCacheHelper, FilterDirectoryReader, SubReaderWrapper,
};
use crate::core::index::index_commit::IndexCommit;
use crate::core::index::index_reader::{
  CacheHelper, CacheKey, CompositeReaderContextKind, IndexReader, IndexReaderBase,
  LeafReaderContextKind,
};
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::leaf_metadata::LeafMetaData;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::pending_soft_deletes::apply_soft_deletes;
use crate::core::index::term::Term;
use crate::core::search::field_exists_query::get_doc_values_doc_id_set_iterator;
use crate::core::search::knn_collector::KnnCollector;
use crate::core::util::bits::Bits;
use crate::core::util::dummy::dummy_comparator::DummyComparator;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::fixed_bit_set::FixedBitSet;
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

/// This reader filters out documents that have a doc values value in the given field and treats
/// these documents as soft deleted. Hard deleted documents are also filtered out in the live docs of
/// this reader.
pub struct SoftDeletesDirectoryReaderWrapper<DR>
where
  DR: DirectoryReader,
  DR::LeafReader: CodecReader + Clone,
  <DR::LeafReader as IndexReader>::ReaderCacheHelper: Clone,
  DR::ReaderCacheHelper: Clone,
{
  in_: DR,
  field: Arc<str>,
  base: BaseCompositeReaderBase<SoftDeletesCodecReader<DR::LeafReader>>,
  index_base: IndexReaderBase,
  reader_cache_helper: Option<DelegatingCacheHelper<DR::ReaderCacheHelper>>,
}

impl<DR> SoftDeletesDirectoryReaderWrapper<DR>
where
  DR: DirectoryReader,
  DR::LeafReader: CodecReader + Clone,
  <DR::LeafReader as IndexReader>::ReaderCacheHelper: Clone,
  DR::ReaderCacheHelper: Clone,
{
  /// Creates a new soft deletes wrapper.
  pub fn new(in_: DR, field: &str) -> Result<Self> {
    Self::new_with_wrapper(
      in_,
      SoftDeletesSubReaderWrapper::new(HashMap::new(), Arc::from(field))?,
    )
  }

  fn new_with_wrapper(
    in_: DR,
    wrapper: SoftDeletesSubReaderWrapper<DR::LeafReader>,
  ) -> Result<Self> {
    let wrapped_readers = wrapper.wrap_readers(in_.get_sequential_sub_readers())?;
    let index_base = IndexReaderBase::new();
    let base = BaseCompositeReaderBase::new::<DummyComparator>(wrapped_readers, None, &index_base)?;
    let reader_cache_helper = in_
      .get_reader_cache_helper()?
      .map(DelegatingCacheHelper::new);

    Ok(Self {
      in_,
      field: wrapper.field,
      base,
      index_base,
      reader_cache_helper,
    })
  }

  fn do_wrap_directory_reader_impl(&self, in_: DR) -> Result<Self> {
    let sub_readers = self.get_sequential_sub_readers();
    let mut reader_cache = HashMap::with_capacity(sub_readers.len());
    for reader in sub_readers {
      if let SoftDeletesCodecReader::B(reader) = reader
        && let Some(reader_cache_helper) = reader.get_delegate().get_reader_cache_helper()?
      {
        reader_cache.insert(
          reader_cache_helper.get_key(),
          SoftDeletesCodecReader::B(reader.clone()),
        );
      }
    }
    SoftDeletesDirectoryReaderWrapper::new_with_wrapper(
      in_,
      SoftDeletesSubReaderWrapper::new(reader_cache, self.field.clone())?,
    )
  }
}

impl<DR> BaseCompositeReader for SoftDeletesDirectoryReaderWrapper<DR>
where
  DR: DirectoryReader,
  DR::LeafReader: CodecReader + Clone,
  <DR::LeafReader as IndexReader>::ReaderCacheHelper: Clone,
  DR::ReaderCacheHelper: Clone,
{
}

impl<DR> CompositeReader for SoftDeletesDirectoryReaderWrapper<DR>
where
  DR: DirectoryReader,
  DR::LeafReader: CodecReader + Clone,
  <DR::LeafReader as IndexReader>::ReaderCacheHelper: Clone,
  DR::ReaderCacheHelper: Clone,
{
  type LeafReader = SoftDeletesCodecReader<DR::LeafReader>;
  type SubReader = Self::LeafReader;

  fn get_sequential_sub_readers(&self) -> &[Self::SubReader] {
    self.base.get_sequential_sub_readers()
  }

  fn visit_leaves<F>(&self, visitor: &mut F) -> Result<()>
  where
    F: FnMut(&Self::LeafReader) -> Result<()>,
  {
    for leaf_reader in self.get_sequential_sub_readers() {
      visitor(leaf_reader)?;
    }
    Ok(())
  }

  fn to_string(&self) -> String {
    let mut buffer = self.in_.to_string();
    buffer.insert_str(0, "SoftDeletesDirectoryReaderWrapper(");
    buffer.push(')');
    buffer
  }
}

impl<DR> IndexReader for SoftDeletesDirectoryReaderWrapper<DR>
where
  DR: DirectoryReader,
  DR::LeafReader: CodecReader + Clone,
  <DR::LeafReader as IndexReader>::ReaderCacheHelper: Clone,
  DR::ReaderCacheHelper: Clone,
{
  type ContextKind = CompositeReaderContextKind;

  type TermVectors = BCRTermVectorsImpl<<Self as CompositeReader>::LeafReader>;

  fn term_vectors(&self) -> Result<Self::TermVectors> {
    self.base.term_vector(self)
  }

  fn max_doc(&self) -> Result<i32> {
    Ok(self.base.max_doc())
  }

  fn num_docs(&self) -> Result<i32> {
    self.base.num_docs()
  }

  type StoredFields = BCRStoredFieldsImpl<<Self as CompositeReader>::LeafReader>;

  fn stored_fields(&self) -> Result<Self::StoredFields> {
    self.base.stored_fields(self)
  }

  fn do_close(&self) -> Result<()> {
    self.in_.close()
  }

  type ReaderCacheHelper = DelegatingCacheHelper<DR::ReaderCacheHelper>;

  fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
    Ok(self.reader_cache_helper.clone())
  }

  fn doc_freq(&self, term: &Term) -> Result<i32> {
    self.base.doc_freq(term, self)
  }

  fn total_term_freq(&self, term: &Term) -> Result<i64> {
    self.base.total_term_freq(term, self)
  }

  fn get_sum_doc_freq(&self, field: &str) -> Result<i64> {
    self.base.get_sum_doc_freq(field, self)
  }

  fn get_doc_count(&self, field: &str) -> Result<i32> {
    self.base.get_doc_count(field, self)
  }

  fn get_sum_total_term_freq(&self, field: &str) -> Result<i64> {
    self.base.get_sum_total_term_freq(field, self)
  }

  fn index_base(&self) -> &IndexReaderBase {
    &self.index_base
  }
}

impl<DR> Display for SoftDeletesDirectoryReaderWrapper<DR>
where
  DR: DirectoryReader,
  DR::LeafReader: CodecReader + Clone,
  <DR::LeafReader as IndexReader>::ReaderCacheHelper: Clone,
  DR::ReaderCacheHelper: Clone,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", CompositeReader::to_string(self))
  }
}

impl<DR> DirectoryReader for SoftDeletesDirectoryReaderWrapper<DR>
where
  DR: DirectoryReader<DirectoryReader = DR>,
  DR::LeafReader: CodecReader + Clone,
  <DR::LeafReader as IndexReader>::ReaderCacheHelper: Clone,
  DR::ReaderCacheHelper: Clone,
{
  type DirectoryReader = Self;

  fn do_open_if_changed(&self) -> Result<Option<Self::DirectoryReader>> {
    match self.in_.do_open_if_changed()? {
      Some(reader) => Ok(Some(self.do_wrap_directory_reader_impl(reader)?)),
      None => Ok(None),
    }
  }

  fn do_open_if_changed_with_commit<IC>(
    &self,
    commit: Option<&IC>,
  ) -> Result<Option<Self::DirectoryReader>>
  where
    IC: IndexCommit<Directory = Arc<Self::Directory>>,
  {
    match self.in_.do_open_if_changed_with_commit(commit)? {
      Some(reader) => Ok(Some(self.do_wrap_directory_reader_impl(reader)?)),
      None => Ok(None),
    }
  }

  fn do_open_if_changed_with_deletes(
    &self,
    writer: &Arc<IndexWriter<Self::Directory>>,
    apply_deletes: bool,
  ) -> Result<Option<Self::DirectoryReader>> {
    match self
      .in_
      .do_open_if_changed_with_deletes(writer, apply_deletes)?
    {
      Some(reader) => Ok(Some(self.do_wrap_directory_reader_impl(reader)?)),
      None => Ok(None),
    }
  }

  fn get_version(&self) -> Result<i64> {
    self.in_.get_version()
  }

  fn is_current(&self) -> Result<bool> {
    self.in_.is_current()
  }

  type IndexCommit = DR::IndexCommit;

  fn get_index_commit(&self) -> Result<Self::IndexCommit> {
    self.in_.get_index_commit()
  }

  type Directory = DR::Directory;

  fn directory(&self) -> &DirectoryReaderBase<Self::Directory> {
    self.in_.directory()
  }
}

impl<DR> FilterDirectoryReader for SoftDeletesDirectoryReaderWrapper<DR>
where
  DR: DirectoryReader<DirectoryReader = DR>,
  DR::LeafReader: CodecReader + Clone,
  <DR::LeafReader as IndexReader>::ReaderCacheHelper: Clone,
  DR::ReaderCacheHelper: Clone,
{
  type Delegate = DR;

  fn get_delegate(&self) -> &Self::Delegate {
    &self.in_
  }

  type WrapDirectoryReader = Self::DirectoryReader;

  fn do_wrap_directory_reader(
    &self,
    in_: Option<<Self::Delegate as DirectoryReader>::DirectoryReader>,
  ) -> Result<Option<Self::WrapDirectoryReader>> {
    match in_ {
      Some(reader) => Ok(Some(self.do_wrap_directory_reader_impl(reader)?)),
      None => Ok(None),
    }
  }
}

struct SoftDeletesSubReaderWrapper<LR>
where
  LR: CodecReader + Clone,
  LR::ReaderCacheHelper: Clone,
{
  mapping: HashMap<CacheKey, SoftDeletesCodecReader<LR>>,
  field: Arc<str>,
}

impl<LR> SoftDeletesSubReaderWrapper<LR>
where
  LR: CodecReader + Clone,
  LR::ReaderCacheHelper: Clone,
{
  fn new(mapping: HashMap<CacheKey, SoftDeletesCodecReader<LR>>, field: Arc<str>) -> Result<Self> {
    Ok(Self { mapping, field })
  }
}

impl<LR> SubReaderWrapper<LR> for SoftDeletesSubReaderWrapper<LR>
where
  LR: CodecReader + Clone,
  LR::ReaderCacheHelper: Clone,
{
  type LeafReader1 = SoftDeletesCodecReader<LR>;

  fn wrap_readers(&self, readers: &[LR]) -> Result<Vec<SoftDeletesCodecReader<LR>>> {
    let mut wrapped = Vec::with_capacity(readers.len());
    for reader in readers {
      let wrap = self.wrap(reader.clone())?;
      if wrap.num_docs()? != 0 {
        wrapped.push(wrap);
      }
    }
    Ok(wrapped)
  }

  type LeafReader2 = SoftDeletesCodecReader<LR>;

  fn wrap(&self, reader: LR) -> Result<SoftDeletesCodecReader<LR>> {
    if let Some(reader_cache_helper) = reader.get_reader_cache_helper()?
      && let Some(cached_reader) = self.mapping.get(&reader_cache_helper.get_key())
    {
      return Ok(cached_reader.clone());
    }
    wrap(reader, &self.field)
  }
}

/// Wrap a codec reader with soft-deletes live docs for the provided field.
pub(crate) fn wrap<CR>(reader: CR, field: &str) -> Result<SoftDeletesCodecReader<CR>>
where
  CR: CodecReader + Clone,
  CR::ReaderCacheHelper: Clone,
{
  let mut iterator = match get_doc_values_doc_id_set_iterator(field, &reader)? {
    Some(iterator) => iterator,
    None => return Ok(SoftDeletesCodecReader::A(reader)),
  };

  let max_doc = reader.max_doc()?;
  let mut bits = match reader.get_live_docs()? {
    Some(live_docs) => live_docs.copy_of()?,
    None => {
      let max_doc_usize = max_doc as usize;
      let mut bits = FixedBitSet::new(max_doc_usize);
      if max_doc > 0 {
        bits.set_with_range(0, max_doc_usize);
      }
      bits
    },
  };

  let num_soft_deletes = apply_soft_deletes(&mut iterator, &mut bits, |_| Ok(true))?;
  if num_soft_deletes == 0 {
    return Ok(SoftDeletesCodecReader::A(reader));
  }

  let num_deletes = reader.num_deleted_docs()? + num_soft_deletes;
  let num_docs = max_doc - num_deletes;
  debug_assert!(assert_doc_counts(num_docs, num_soft_deletes, &reader)?);
  Ok(SoftDeletesCodecReader::B(
    SoftDeletesFilterCodecReader::new(reader, bits, num_docs)?,
  ))
}

fn assert_doc_counts<LR>(
  _expected_num_docs: i32,
  _num_soft_deletes: i32,
  _reader: &LR,
) -> Result<bool>
where
  LR: LeafReader,
{
  Ok(true)
}

/// A leaf reader with live docs that additionally filter soft-deleted documents.
pub struct SoftDeletesFilterLeafReader<LR>
where
  LR: LeafReader,
  LR::ReaderCacheHelper: Clone,
{
  reader: LR,
  bits: Arc<FixedBitSet>,
  num_docs: i32,
  index_base: IndexReaderBase,
  reader_cache_helper: Option<DelegatingCacheHelper<LR::ReaderCacheHelper>>,
}

impl<LR> SoftDeletesFilterLeafReader<LR>
where
  LR: LeafReader,
  LR::ReaderCacheHelper: Clone,
{
  fn new(reader: LR, bits: FixedBitSet, num_docs: i32) -> Result<Self> {
    let index_base = IndexReaderBase::new();
    reader.register_parent_reader(&index_base)?;
    let reader_cache_helper = reader
      .get_reader_cache_helper()?
      .map(DelegatingCacheHelper::new);
    Ok(Self {
      reader,
      bits: Arc::new(bits),
      num_docs,
      index_base,
      reader_cache_helper,
    })
  }

  pub fn get_delegate(&self) -> &LR {
    &self.reader
  }
}

impl<LR> Clone for SoftDeletesFilterLeafReader<LR>
where
  LR: LeafReader + Clone,
  LR::ReaderCacheHelper: Clone,
{
  fn clone(&self) -> Self {
    Self {
      reader: self.reader.clone(),
      bits: self.bits.clone(),
      num_docs: self.num_docs,
      index_base: self.index_base.clone(),
      reader_cache_helper: self.reader_cache_helper.clone(),
    }
  }
}

impl<LR> Display for SoftDeletesFilterLeafReader<LR>
where
  LR: LeafReader,
  LR::ReaderCacheHelper: Clone,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "SoftDeletesFilterLeafReader({})", self.reader)
  }
}

impl<LR> IndexReader for SoftDeletesFilterLeafReader<LR>
where
  LR: LeafReader,
  LR::ReaderCacheHelper: Clone,
{
  type ContextKind = LeafReaderContextKind;

  type TermVectors = LR::TermVectors;

  fn term_vectors(&self) -> Result<Self::TermVectors> {
    self.ensure_open()?;
    self.reader.term_vectors()
  }

  fn max_doc(&self) -> Result<i32> {
    self.reader.max_doc()
  }

  fn num_docs(&self) -> Result<i32> {
    Ok(self.num_docs)
  }

  type StoredFields = LR::StoredFields;

  fn stored_fields(&self) -> Result<Self::StoredFields> {
    self.ensure_open()?;
    self.reader.stored_fields()
  }

  fn do_close(&self) -> Result<()> {
    self.reader.close()
  }

  type ReaderCacheHelper = DelegatingCacheHelper<LR::ReaderCacheHelper>;

  fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
    Ok(self.reader_cache_helper.clone())
  }

  fn doc_freq(&self, term: &Term) -> Result<i32> {
    LeafReader::doc_freq(self, term)
  }

  fn total_term_freq(&self, term: &Term) -> Result<i64> {
    self.get_total_term_freq(term)
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

impl<LR> LeafReader for SoftDeletesFilterLeafReader<LR>
where
  LR: LeafReader,
  LR::ReaderCacheHelper: Clone,
{
  type CacheHelper = LR::CacheHelper;

  fn get_core_cache_helper(&self) -> Result<Option<Self::CacheHelper>> {
    self.reader.get_core_cache_helper()
  }

  type Terms = LR::Terms;

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    self.ensure_open()?;
    self.reader.terms(field)
  }

  type NumericDocValues = LR::NumericDocValues;

  fn get_numeric_doc_values(&self, field: &str) -> Result<Option<Self::NumericDocValues>> {
    self.ensure_open()?;
    self.reader.get_numeric_doc_values(field)
  }

  type BinaryDocValues = LR::BinaryDocValues;

  fn get_binary_doc_values(&self, field: &str) -> Result<Option<Self::BinaryDocValues>> {
    self.ensure_open()?;
    self.reader.get_binary_doc_values(field)
  }

  type SortedDocValues = LR::SortedDocValues;

  fn get_sorted_doc_values(&self, field: &str) -> Result<Option<Self::SortedDocValues>> {
    self.ensure_open()?;
    self.reader.get_sorted_doc_values(field)
  }

  type SortedNumericDocValues = LR::SortedNumericDocValues;

  fn get_sorted_numeric_doc_values(
    &self,
    field: &str,
  ) -> Result<Option<Self::SortedNumericDocValues>> {
    self.ensure_open()?;
    self.reader.get_sorted_numeric_doc_values(field)
  }

  type SortedSetDocValues = LR::SortedSetDocValues;

  fn get_sorted_set_doc_values(&self, field: &str) -> Result<Option<Self::SortedSetDocValues>> {
    self.ensure_open()?;
    self.reader.get_sorted_set_doc_values(field)
  }

  type NormNumericDocValues = LR::NormNumericDocValues;

  fn get_norm_values(&self, field: &str) -> Result<Option<Self::NormNumericDocValues>> {
    self.ensure_open()?;
    self.reader.get_norm_values(field)
  }

  type DocValuesSkipper = LR::DocValuesSkipper;

  fn get_doc_values_skipper(&self, field: &str) -> Result<Option<Self::DocValuesSkipper>> {
    self.ensure_open()?;
    self.reader.get_doc_values_skipper(field)
  }

  type FloatVectorValues = LR::FloatVectorValues;

  fn get_float_vector_values(&self, field: &str) -> Result<Option<Self::FloatVectorValues>> {
    self.reader.get_float_vector_values(field)
  }

  type ByteVectorValues = LR::ByteVectorValues;

  fn get_byte_vector_values(&self, field: &str) -> Result<Option<Self::ByteVectorValues>> {
    self.reader.get_byte_vector_values(field)
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
    self
      .reader
      .search_nearest_vectors_f32(field, target, knn_collector, accept_docs)
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
    self
      .reader
      .search_nearest_vectors_u8(field, target, knn_collector, accept_docs)
  }

  fn get_field_infos(&self) -> Result<Arc<crate::core::index::field_infos::FieldInfos>> {
    self.reader.get_field_infos()
  }

  type Bits = Arc<FixedBitSet>;

  fn get_live_docs(&self) -> Result<Option<Self::Bits>> {
    Ok(Some(self.bits.clone()))
  }

  type PointValues = LR::PointValues;

  fn get_point_values(&self, field: &str) -> Result<Option<Self::PointValues>> {
    self.reader.get_point_values(field)
  }

  fn check_integrity(&self) -> Result<()> {
    self.ensure_open()?;
    self.reader.check_integrity()
  }

  fn get_metadata(&self) -> Result<&LeafMetaData> {
    self.ensure_open()?;
    self.reader.get_metadata()
  }
}

/// A codec reader with live docs that additionally filter soft-deleted documents.
pub struct SoftDeletesFilterCodecReader<CR>
where
  CR: CodecReader,
  CR::ReaderCacheHelper: Clone,
{
  base: SoftDeletesFilterLeafReader<CR>,
}

impl<CR> SoftDeletesFilterCodecReader<CR>
where
  CR: CodecReader,
  CR::ReaderCacheHelper: Clone,
{
  fn new(reader: CR, bits: FixedBitSet, num_docs: i32) -> Result<Self> {
    Ok(Self {
      base: SoftDeletesFilterLeafReader::new(reader, bits, num_docs)?,
    })
  }

  pub fn get_delegate(&self) -> &CR {
    self.base.get_delegate()
  }
}

impl<CR> Clone for SoftDeletesFilterCodecReader<CR>
where
  CR: CodecReader + Clone,
  CR::ReaderCacheHelper: Clone,
{
  fn clone(&self) -> Self {
    Self {
      base: self.base.clone(),
    }
  }
}

impl<CR> Display for SoftDeletesFilterCodecReader<CR>
where
  CR: CodecReader,
  CR::ReaderCacheHelper: Clone,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "SoftDeletesFilterCodecReader({})", self.base.reader)
  }
}

impl<CR> IndexReader for SoftDeletesFilterCodecReader<CR>
where
  CR: CodecReader,
  CR::ReaderCacheHelper: Clone,
{
  type ContextKind = LeafReaderContextKind;

  type TermVectors = CR::TermVectors;

  fn term_vectors(&self) -> Result<Self::TermVectors> {
    IndexReader::term_vectors(&self.base.reader)
  }

  fn max_doc(&self) -> Result<i32> {
    self.base.reader.max_doc()
  }

  fn num_docs(&self) -> Result<i32> {
    Ok(self.base.num_docs)
  }

  type StoredFields = CR::StoredFields;

  fn stored_fields(&self) -> Result<Self::StoredFields> {
    IndexReader::stored_fields(&self.base.reader)
  }

  fn do_close(&self) -> Result<()> {
    self.base.reader.do_close()
  }

  type ReaderCacheHelper = DelegatingCacheHelper<CR::ReaderCacheHelper>;

  fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
    Ok(self.base.reader_cache_helper.clone())
  }

  fn doc_freq(&self, term: &Term) -> Result<i32> {
    IndexReader::doc_freq(&self.base.reader, term)
  }

  fn total_term_freq(&self, term: &Term) -> Result<i64> {
    self.base.reader.total_term_freq(term)
  }

  fn get_sum_doc_freq(&self, field: &str) -> Result<i64> {
    IndexReader::get_sum_doc_freq(&self.base.reader, field)
  }

  fn get_doc_count(&self, field: &str) -> Result<i32> {
    IndexReader::get_doc_count(&self.base.reader, field)
  }

  fn get_sum_total_term_freq(&self, field: &str) -> Result<i64> {
    IndexReader::get_sum_total_term_freq(&self.base.reader, field)
  }

  fn index_base(&self) -> &IndexReaderBase {
    &self.base.index_base
  }
}

impl<CR> LeafReader for SoftDeletesFilterCodecReader<CR>
where
  CR: CodecReader,
  CR::ReaderCacheHelper: Clone,
{
  type CacheHelper = CR::CacheHelper;

  fn get_core_cache_helper(&self) -> Result<Option<Self::CacheHelper>> {
    LeafReader::get_core_cache_helper(&self.base.reader)
  }

  type Terms = CR::Terms;

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    LeafReader::terms(&self.base.reader, field)
  }

  type NumericDocValues = CR::NumericDocValues;

  fn get_numeric_doc_values(&self, field: &str) -> Result<Option<Self::NumericDocValues>> {
    LeafReader::get_numeric_doc_values(&self.base.reader, field)
  }

  type BinaryDocValues = CR::BinaryDocValues;

  fn get_binary_doc_values(&self, field: &str) -> Result<Option<Self::BinaryDocValues>> {
    LeafReader::get_binary_doc_values(&self.base.reader, field)
  }

  type SortedDocValues = CR::SortedDocValues;

  fn get_sorted_doc_values(&self, field: &str) -> Result<Option<Self::SortedDocValues>> {
    LeafReader::get_sorted_doc_values(&self.base.reader, field)
  }

  type SortedNumericDocValues = CR::SortedNumericDocValues;

  fn get_sorted_numeric_doc_values(
    &self,
    field: &str,
  ) -> Result<Option<Self::SortedNumericDocValues>> {
    LeafReader::get_sorted_numeric_doc_values(&self.base.reader, field)
  }

  type SortedSetDocValues = CR::SortedSetDocValues;

  fn get_sorted_set_doc_values(&self, field: &str) -> Result<Option<Self::SortedSetDocValues>> {
    LeafReader::get_sorted_set_doc_values(&self.base.reader, field)
  }

  type NormNumericDocValues = CR::NormNumericDocValues;

  fn get_norm_values(&self, field: &str) -> Result<Option<Self::NormNumericDocValues>> {
    LeafReader::get_norm_values(&self.base.reader, field)
  }

  type DocValuesSkipper = CR::DocValuesSkipper;

  fn get_doc_values_skipper(&self, field: &str) -> Result<Option<Self::DocValuesSkipper>> {
    LeafReader::get_doc_values_skipper(&self.base.reader, field)
  }

  type FloatVectorValues = CR::FloatVectorValues;

  fn get_float_vector_values(&self, field: &str) -> Result<Option<Self::FloatVectorValues>> {
    LeafReader::get_float_vector_values(&self.base.reader, field)
  }

  type ByteVectorValues = CR::ByteVectorValues;

  fn get_byte_vector_values(&self, field: &str) -> Result<Option<Self::ByteVectorValues>> {
    LeafReader::get_byte_vector_values(&self.base.reader, field)
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
    LeafReader::search_nearest_vectors_f32(
      &self.base.reader,
      field,
      target,
      knn_collector,
      accept_docs,
    )
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
    LeafReader::search_nearest_vectors_u8(
      &self.base.reader,
      field,
      target,
      knn_collector,
      accept_docs,
    )
  }

  fn get_field_infos(&self) -> Result<Arc<crate::core::index::field_infos::FieldInfos>> {
    LeafReader::get_field_infos(&self.base.reader)
  }

  type Bits = Arc<FixedBitSet>;

  fn get_live_docs(&self) -> Result<Option<Self::Bits>> {
    Ok(Some(self.base.bits.clone()))
  }

  type PointValues = CR::PointValues;

  fn get_point_values(&self, field: &str) -> Result<Option<Self::PointValues>> {
    LeafReader::get_point_values(&self.base.reader, field)
  }

  fn check_integrity(&self) -> Result<()> {
    LeafReader::check_integrity(&self.base.reader)
  }

  fn get_metadata(&self) -> Result<&LeafMetaData> {
    LeafReader::get_metadata(&self.base.reader)
  }
}

impl<CR> CodecReader for SoftDeletesFilterCodecReader<CR>
where
  CR: CodecReader,
  CR::ReaderCacheHelper: Clone,
{
  type StoredFieldsReader = CR::StoredFieldsReader;
  type TermVectorsReader = CR::TermVectorsReader;
  type NormsProducer = CR::NormsProducer;
  type DocValuesProducer = CR::DocValuesProducer;
  type FieldsProducer = CR::FieldsProducer;
  type PointsReader = CR::PointsReader;
  type KnnVectorsReader = CR::KnnVectorsReader;

  fn get_fields_reader(&self) -> Result<Option<Self::StoredFieldsReader>> {
    self.base.reader.get_fields_reader()
  }

  fn get_term_vectors_reader(&self) -> Result<Option<Self::TermVectorsReader>> {
    self.base.reader.get_term_vectors_reader()
  }

  fn get_norms_reader(&self) -> Result<Option<Self::NormsProducer>> {
    self.base.reader.get_norms_reader()
  }

  fn get_doc_values_reader(&self) -> Result<Option<Self::DocValuesProducer>> {
    self.base.reader.get_doc_values_reader()
  }

  fn get_postings_reader(&self) -> Result<Option<Self::FieldsProducer>> {
    self.base.reader.get_postings_reader()
  }

  fn get_points_reader(&self) -> Result<Option<Self::PointsReader>> {
    self.base.reader.get_points_reader()
  }

  fn get_vector_reader(&self) -> Result<Option<Self::KnnVectorsReader>> {
    self.base.reader.get_vector_reader()
  }
}
