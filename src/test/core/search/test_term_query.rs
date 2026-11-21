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
use crate::core::document::document::Document;
use crate::core::document::field::Store;
use crate::core::document::string_field::StringField;
use crate::core::index::BytesRef;
use crate::core::index::base_composite_reader::{
  BCRStoredFieldsImpl, BCRTermVectorsImpl, BaseCompositeReader, BaseCompositeReaderBase,
};
use crate::core::index::composite_reader::CompositeReader;
use crate::core::index::directory_reader::{DirectoryReader, DirectoryReaderBase};
use crate::core::index::field_invert_state::FieldInvertState;
use crate::core::index::filter_directory_reader::{FilterDirectoryReader, SubReaderWrapper};
use crate::core::index::index_reader::{
  CompositeReaderContextKind, IndexReader, IndexReaderBase, LeafReaderContextKind,
};
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::leaf_metadata::LeafMetaData;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::multi_reader::MultiReader;
use crate::core::index::no_merge_policy::NoMergePolicy;
use crate::core::index::term::Term;
use crate::core::index::term_states::build;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::{SeekStatus, TermsEnum};
use crate::core::search::collection_statistics::CollectionStatistics;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::knn_collector::KnnCollector;
use crate::core::search::query::{Query, QueryBase};
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::similarities_impl::similarities::{BoxSimScorer, SimScorer, Similarity};
use crate::core::search::term_query::TermQuery;
use crate::core::search::term_statistics::TermStatistics;
use crate::core::util::access::ByteSource;
use crate::core::util::bits::Bits;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::close::CloseableRef;
use crate::core::util::dummy::dummy_comparator::DummyComparator;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::io_utils::IOUtils;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::search::dummy_total_hit_count_collector::DummyTotalHitCountCollector;
use crate::test_framework::core::search::query_utils::QueryUtils;
use crate::test_framework::core::util::lucene_test_case::{
  new_directory_shared, new_index_writer_config, new_searcher_with_reader, random,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::RngExt;
use std::borrow::Cow;
use std::fmt::{Display, Formatter};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[allow(dead_code)] // for quick search
struct TestTermQuery;

#[test]
fn test_equals() -> Result<()> {
  QueryUtils::check_equal::<Query>(
    &TermQuery::new(Term::from_text("foo", "bar")).into(),
    &TermQuery::new(Term::from_text("foo", "bar")).into(),
  );

  QueryUtils::check_unequal::<Query>(
    &TermQuery::new(Term::from_text("foo", "bar")).into(),
    &TermQuery::new(Term::from_text("foo", "baz")).into(),
  );

  let multi_reader = MultiReader::empty()?;
  let context = multi_reader.get_context()?;
  let searcher = IndexSearcher::new(context)?;

  QueryUtils::check_equal::<Query>(
    &TermQuery::new(Term::from_text("foo", "bar")).into(),
    &TermQuery::with_term_state(
      Term::from_text("foo", "bar"),
      Some(Arc::new(build(
        &searcher,
        Term::from_text("foo", "bar"),
        true,
      )?)),
    )
    .into(),
  );

  searcher.get_index_reader().close()
}
#[test]
fn test_create_weight_does_not_seek_if_scores_are_not_needed() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut config = new_index_writer_config(&mut random)?;
  config.set_merge_policy(NoMergePolicy::default());
  let writer = RandomIndexWriter::with_config(&mut random, dir.clone(), config);

  // segment that contains the term
  let mut doc = Document::new();
  doc.add(StringField::from_string("foo", "bar", Store::No)?);
  writer.add_document(&mut random, doc)?;
  writer.get_reader(&mut random)?.close()?;

  // segment that does not contain the term
  let mut doc = Document::new();
  doc.add(StringField::from_string("foo", "baz", Store::No)?);
  writer.add_document(&mut random, doc)?;
  writer.get_reader(&mut random)?.close()?;

  // segment that does not contain the field
  writer.add_document(&mut random, Document::new())?;

  let reader = Arc::new(writer.get_reader(&mut random)?);
  let no_seek_reader = Arc::new(NoSeekDirectoryReader::new(reader.clone())?);
  let no_seek_searcher = IndexSearcher::new(no_seek_reader.clone().get_context()?)?;
  let query: Query = TermQuery::new(Term::from_text("foo", "bar")).into();

  let error = catch_unwind(AssertUnwindSafe(|| -> Result<()> {
    no_seek_searcher.create_weight(
      no_seek_searcher.rewrite(query.clone())?,
      ScoreMode::Complete,
      1.0,
    )?;
    Ok(())
  }))
  .expect_err("ScoreMode::Complete should seek terms");
  assert_eq!(
    "no seek",
    LuceneError::panic_payload_message(error.as_ref())
  );

  no_seek_searcher.create_weight(
    no_seek_searcher.rewrite(query.clone())?,
    ScoreMode::CompleteNoScores,
    1.0,
  )?; // no error

  let searcher = IndexSearcher::new(reader.clone().get_context()?)?;
  // Use a collector rather than searcher.count(), which would just read the
  // doc freq instead of creating a scorer.
  assert_eq!(
    1,
    searcher
      .search_with_collector_manager(query, &DummyTotalHitCountCollector::create_manager())?
  );
  let query_with_context: Query = TermQuery::with_term_state(
    Term::from_text("foo", "bar"),
    Some(Arc::new(build(
      &searcher,
      Term::from_text("foo", "bar"),
      true,
    )?)),
  )
  .into();
  assert_eq!(
    1,
    searcher.search_with_collector_manager(
      query_with_context,
      &DummyTotalHitCountCollector::create_manager(),
    )?
  );

  drop(no_seek_searcher);
  drop(no_seek_reader);
  drop(searcher);
  let close_result = IOUtils::use_or_suppress_result(reader.close(), writer.close(&mut random));
  IOUtils::use_or_suppress_result(close_result, dir.close())
}
#[test]
fn test_query_matches_count() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let writer = RandomIndexWriter::new(&mut random, dir.clone())?;

  let random_num_docs = TestUtil::next_int(&mut random, 10, 100);
  let mut num_matching_docs = 0;

  for _ in 0..random_num_docs {
    let mut doc = Document::new();
    if random.random_bool(0.5) {
      doc.add(StringField::from_string("foo", "bar", Store::No)?);
      num_matching_docs += 1;
    }
    writer.add_document(&mut random, doc)?;
  }

  writer.force_merge(&mut random, 1)?;

  let reader = writer.get_reader(&mut random)?;
  let searcher = new_searcher_with_reader(reader)?;

  let test_query: Query = TermQuery::new(Term::from_text("foo", "bar")).into();
  assert_eq!(num_matching_docs, searcher.count(test_query.clone())?);

  let weight = searcher.create_weight(test_query, ScoreMode::Complete, 1.0)?;
  let leaves = searcher.reader_context.leaves()?;
  assert_eq!(num_matching_docs, weight.count(&leaves[0], &searcher)?);

  let close_result = IOUtils::use_or_suppress_result(
    searcher.get_index_reader().close(),
    writer.close(&mut random),
  );
  IOUtils::use_or_suppress_result(close_result, dir.close())
}
#[test]
fn test_get_term_states() -> Result<()> {
  assert!(
    TermQuery::new(Term::from_text("foo", "bar"))
      .get_term_states()
      .is_none()
  );

  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let mut iwc = new_index_writer_config(&mut random)?;
  iwc.set_merge_policy(NoMergePolicy::default());

  let writer = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);

  let mut doc = Document::new();
  doc.add(StringField::from_string("foo", "bar", Store::No)?);
  writer.add_document(&mut random, doc)?;
  writer.get_reader(&mut random)?.close()?;

  let mut doc = Document::new();
  doc.add(StringField::from_string("foo", "baz", Store::No)?);
  writer.add_document(&mut random, doc)?;
  writer.get_reader(&mut random)?.close()?;

  writer.add_document(&mut random, Document::new())?;

  let reader = writer.get_reader(&mut random)?;
  let searcher = new_searcher_with_reader(reader)?;

  let query_with_context = TermQuery::with_term_state(
    Term::from_text("foo", "bar"),
    Some(Arc::new(build(
      &searcher,
      Term::from_text("foo", "bar"),
      true,
    )?)),
  );
  assert!(query_with_context.get_term_states().is_some());

  let close_result = IOUtils::use_or_suppress_result(
    searcher.get_index_reader().close(),
    writer.close(&mut random),
  );
  IOUtils::use_or_suppress_result(close_result, dir.close())
}

#[test]
fn test_with_with_different_score_modes() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let mut iwc = new_index_writer_config(&mut random)?;
  iwc.set_merge_policy(NoMergePolicy::default());
  let writer = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);

  let mut doc = Document::new();
  doc.add(StringField::from_string("foo", "bar", Store::No)?);
  writer.add_document(&mut random, doc)?;
  writer.get_reader(&mut random)?.close()?;

  let reader = writer.get_reader(&mut random)?;
  let searcher = new_searcher_with_reader(Arc::new(reader))?;
  let existing_similarity = searcher.get_similarity();
  let term = Arc::new(Term::from_text("foo", "bar"));

  for score_mode in ScoreMode::values() {
    let scorer_called = Arc::new(AtomicBool::new(false));
    let s = SimilarityImpl::new(existing_similarity.clone(), scorer_called.clone());
    let searcher =
      crate::test_framework::core::util::lucene_test_case::new_searcher_with_reader_and_similarity(
        searcher.get_index_reader().clone(),
        Arc::new(s),
      )?;
    let term_query = TermQuery::new(term.clone());
    term_query.create_weight(&searcher, score_mode, 1f32)?;
    assert_eq!(
      score_mode.needs_scores(),
      scorer_called.load(Ordering::SeqCst)
    );
  }

  let close_result = IOUtils::use_or_suppress_result(
    searcher.get_index_reader().close(),
    writer.close(&mut random),
  );
  IOUtils::use_or_suppress_result(close_result, dir.close())
}

struct NoSeekDirectoryReader<DR>
where
  DR: DirectoryReader,
{
  in_: DR,
  base: BaseCompositeReaderBase<NoSeekLeafReader<DR::LeafReader>>,
  index_base: IndexReaderBase,
}

impl<DR> NoSeekDirectoryReader<DR>
where
  DR: DirectoryReader,
{
  fn new(in_: DR) -> Result<Self> {
    let wrapper = NoSeekSubReaderWrapper;
    let readers = wrapper.wrap_readers(in_.get_sequential_sub_readers())?;
    let index_base = IndexReaderBase::new();
    let base = BaseCompositeReaderBase::new::<DummyComparator>(readers, None, &index_base)?;
    Ok(Self {
      in_,
      base,
      index_base,
    })
  }
}

impl<DR> BaseCompositeReader for NoSeekDirectoryReader<DR> where DR: DirectoryReader {}

impl<DR> CompositeReader for NoSeekDirectoryReader<DR>
where
  DR: DirectoryReader,
{
  type LeafReader = NoSeekLeafReader<DR::LeafReader>;
  type SubReader = Self::LeafReader;

  fn get_sequential_sub_readers(&self) -> &[Self::SubReader] {
    self.base.get_sequential_sub_readers()
  }

  fn visit_leaves<F>(&self, visitor: &mut F) -> Result<()>
  where
    F: FnMut(&Self::LeafReader) -> Result<()>,
  {
    for reader in self.get_sequential_sub_readers() {
      visitor(reader)?;
    }
    Ok(())
  }

  fn to_string(&self) -> String {
    format!("NoSeekDirectoryReader({})", self.in_.to_string())
  }
}

impl<DR> IndexReader for NoSeekDirectoryReader<DR>
where
  DR: DirectoryReader,
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

  type ReaderCacheHelper = DR::ReaderCacheHelper;

  fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
    self.in_.get_reader_cache_helper()
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

impl<DR> Display for NoSeekDirectoryReader<DR>
where
  DR: DirectoryReader,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", CompositeReader::to_string(self))
  }
}

impl<DR> DirectoryReader for NoSeekDirectoryReader<DR>
where
  DR: DirectoryReader,
{
  type DirectoryReader = NoSeekDirectoryReader<DR::DirectoryReader>;

  fn do_open_if_changed(&self) -> Result<Option<Self::DirectoryReader>> {
    self.wrap_directory_reader(self.in_.do_open_if_changed()?)
  }

  fn do_open_if_changed_with_commit<IC>(
    &self,
    commit: Option<&IC>,
  ) -> Result<Option<Self::DirectoryReader>>
  where
    IC: crate::core::index::index_commit::IndexCommit<Directory = Arc<Self::Directory>>,
  {
    self.wrap_directory_reader(self.in_.do_open_if_changed_with_commit(commit)?)
  }

  fn do_open_if_changed_with_deletes(
    &self,
    writer: &Arc<IndexWriter<Self::Directory>>,
    apply_deletes: bool,
  ) -> Result<Option<Self::DirectoryReader>> {
    self.wrap_directory_reader(
      self
        .in_
        .do_open_if_changed_with_deletes(writer, apply_deletes)?,
    )
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

impl<DR> FilterDirectoryReader for NoSeekDirectoryReader<DR>
where
  DR: DirectoryReader,
{
  type Delegate = DR;

  fn get_delegate(&self) -> &Self::Delegate {
    &self.in_
  }

  type WrapDirectoryReader = NoSeekDirectoryReader<DR::DirectoryReader>;

  fn do_wrap_directory_reader(
    &self,
    in_: Option<<Self::Delegate as DirectoryReader>::DirectoryReader>,
  ) -> Result<Option<Self::WrapDirectoryReader>> {
    in_.map(NoSeekDirectoryReader::new).transpose()
  }
}

struct NoSeekSubReaderWrapper;

impl<LR> SubReaderWrapper<LR> for NoSeekSubReaderWrapper
where
  LR: LeafReader + Clone,
{
  type LeafReader1 = Self::LeafReader2;

  fn wrap_readers(&self, readers: &[LR]) -> Result<Vec<Self::LeafReader1>> {
    self.default_wrap_readers(readers.to_vec())
  }

  type LeafReader2 = NoSeekLeafReader<LR>;

  fn wrap(&self, reader: LR) -> Result<Self::LeafReader2> {
    NoSeekLeafReader::new(reader)
  }
}

struct NoSeekLeafReader<LR>
where
  LR: LeafReader,
{
  in_: LR,
  index_base: IndexReaderBase,
}

impl<LR> NoSeekLeafReader<LR>
where
  LR: LeafReader,
{
  fn new(in_: LR) -> Result<Self> {
    let index_base = IndexReaderBase::new();
    in_.register_parent_reader(&index_base)?;
    Ok(Self { in_, index_base })
  }
}

impl<LR> Clone for NoSeekLeafReader<LR>
where
  LR: LeafReader + Clone,
{
  fn clone(&self) -> Self {
    Self {
      in_: self.in_.clone(),
      index_base: self.index_base.clone(),
    }
  }
}

impl<LR> Display for NoSeekLeafReader<LR>
where
  LR: LeafReader,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "NoSeekLeafReader({})", self.in_)
  }
}

impl<LR> IndexReader for NoSeekLeafReader<LR>
where
  LR: LeafReader,
{
  type ContextKind = LeafReaderContextKind;
  type TermVectors = LR::TermVectors;

  fn term_vectors(&self) -> Result<Self::TermVectors> {
    self.in_.term_vectors()
  }

  fn max_doc(&self) -> Result<i32> {
    self.in_.max_doc()
  }

  fn num_docs(&self) -> Result<i32> {
    self.in_.num_docs()
  }

  type StoredFields = LR::StoredFields;

  fn stored_fields(&self) -> Result<Self::StoredFields> {
    self.in_.stored_fields()
  }

  fn do_close(&self) -> Result<()> {
    self.in_.close()
  }

  type ReaderCacheHelper = LR::ReaderCacheHelper;

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
    &self.index_base
  }
}

impl<LR> LeafReader for NoSeekLeafReader<LR>
where
  LR: LeafReader,
{
  type CacheHelper = LR::CacheHelper;

  fn get_core_cache_helper(&self) -> Result<Option<Self::CacheHelper>> {
    self.in_.get_core_cache_helper()
  }

  type Terms = NoSeekTerms<LR::Terms>;

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    Ok(self.in_.terms(field)?.map(NoSeekTerms::new))
  }

  type NumericDocValues = LR::NumericDocValues;

  fn get_numeric_doc_values(&self, field: &str) -> Result<Option<Self::NumericDocValues>> {
    self.in_.get_numeric_doc_values(field)
  }

  type BinaryDocValues = LR::BinaryDocValues;

  fn get_binary_doc_values(&self, field: &str) -> Result<Option<Self::BinaryDocValues>> {
    self.in_.get_binary_doc_values(field)
  }

  type SortedDocValues = LR::SortedDocValues;

  fn get_sorted_doc_values(&self, field: &str) -> Result<Option<Self::SortedDocValues>> {
    self.in_.get_sorted_doc_values(field)
  }

  type SortedNumericDocValues = LR::SortedNumericDocValues;

  fn get_sorted_numeric_doc_values(
    &self,
    field: &str,
  ) -> Result<Option<Self::SortedNumericDocValues>> {
    self.in_.get_sorted_numeric_doc_values(field)
  }

  type SortedSetDocValues = LR::SortedSetDocValues;

  fn get_sorted_set_doc_values(&self, field: &str) -> Result<Option<Self::SortedSetDocValues>> {
    self.in_.get_sorted_set_doc_values(field)
  }

  type NormNumericDocValues = LR::NormNumericDocValues;

  fn get_norm_values(&self, field: &str) -> Result<Option<Self::NormNumericDocValues>> {
    self.in_.get_norm_values(field)
  }

  type DocValuesSkipper = LR::DocValuesSkipper;

  fn get_doc_values_skipper(&self, field: &str) -> Result<Option<Self::DocValuesSkipper>> {
    self.in_.get_doc_values_skipper(field)
  }

  type FloatVectorValues = LR::FloatVectorValues;

  fn get_float_vector_values(&self, field: &str) -> Result<Option<Self::FloatVectorValues>> {
    self.in_.get_float_vector_values(field)
  }

  type ByteVectorValues = LR::ByteVectorValues;

  fn get_byte_vector_values(&self, field: &str) -> Result<Option<Self::ByteVectorValues>> {
    self.in_.get_byte_vector_values(field)
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
      .in_
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
      .in_
      .search_nearest_vectors_u8(field, target, knn_collector, accept_docs)
  }

  fn get_field_infos(&self) -> Result<Arc<crate::core::index::field_infos::FieldInfos>> {
    self.in_.get_field_infos()
  }

  type Bits = LR::Bits;

  fn get_live_docs(&self) -> Result<Option<Self::Bits>> {
    self.in_.get_live_docs()
  }

  type PointValues = LR::PointValues;

  fn get_point_values(&self, field: &str) -> Result<Option<Self::PointValues>> {
    self.in_.get_point_values(field)
  }

  fn check_integrity(&self) -> Result<()> {
    self.in_.check_integrity()
  }

  fn get_metadata(&self) -> Result<&LeafMetaData> {
    self.in_.get_metadata()
  }
}

struct NoSeekTerms<T>
where
  T: Terms,
{
  in_: T,
}

impl<T> NoSeekTerms<T>
where
  T: Terms,
{
  fn new(in_: T) -> Self {
    Self { in_ }
  }
}

impl<T> Terms for NoSeekTerms<T>
where
  T: Terms,
{
  type TermsEnum = NoSeekTermsEnum<T::TermsEnum>;

  fn iterator(&self) -> Result<Self::TermsEnum> {
    Ok(NoSeekTermsEnum::new(self.in_.iterator()?))
  }

  type IntersectIter = T::IntersectIter;

  fn intersect(
    &self,
    compiled: &crate::core::util::automation::compiled_automaton::CompiledAutomaton,
    start_term: Option<&BytesRef<Vec<u8>>>,
  ) -> Result<Self::IntersectIter> {
    self.in_.intersect(compiled, start_term)
  }

  fn size(&self) -> Result<i64> {
    self.in_.size()
  }

  fn get_sum_total_term_freq(&self) -> Result<i64> {
    self.in_.get_sum_total_term_freq()
  }

  fn get_sum_doc_freq(&self) -> Result<i64> {
    self.in_.get_sum_doc_freq()
  }

  fn get_doc_count(&self) -> Result<i32> {
    self.in_.get_doc_count()
  }

  fn has_freqs(&self) -> bool {
    self.in_.has_freqs()
  }

  fn has_offsets(&self) -> bool {
    self.in_.has_offsets()
  }

  fn has_positions(&self) -> bool {
    self.in_.has_positions()
  }

  fn has_payloads(&self) -> bool {
    self.in_.has_payloads()
  }

  fn get_stats(&self) -> Result<String> {
    self.in_.get_stats()
  }
}

struct NoSeekTermsEnum<TE>
where
  TE: TermsEnum,
{
  in_: TE,
}

impl<TE> NoSeekTermsEnum<TE>
where
  TE: TermsEnum,
{
  fn new(in_: TE) -> Self {
    Self { in_ }
  }
}

impl<TE> BytesRefIterator for NoSeekTermsEnum<TE>
where
  TE: TermsEnum,
{
  type Value<'a>
    = TE::Value<'a>
  where
    Self: 'a;

  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    self.in_.next()
  }
}

impl<TE> TermsEnum for NoSeekTermsEnum<TE>
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
    self.in_.attributes()
  }

  fn attributes_mut(&mut self) -> Result<Self::AttributeSourceMut<'_>> {
    self.in_.attributes_mut()
  }

  fn seek_exact<BS: ByteSource>(&mut self, _term: &BytesRef<BS>) -> Result<bool> {
    panic!("no seek")
  }

  type PreparedSeek<BS: ByteSource> = crate::core::index::terms_enum::SeekExact;

  fn prepare_seek_exact<BS: ByteSource>(
    &mut self,
    _text: &BytesRef<BS>,
  ) -> Result<Option<Self::PreparedSeek<BS>>> {
    panic!("no seek")
  }

  fn seek_ceil<BS: ByteSource>(&mut self, _term: &BytesRef<BS>) -> Result<SeekStatus> {
    panic!("no seek")
  }

  fn seek_exact_with_ord(&mut self, _ord: i64) -> Result<()> {
    panic!("no seek")
  }

  fn seek_exact_with_state<BS: ByteSource>(
    &mut self,
    _term: &BytesRef<BS>,
    _state: &TermStateEnum,
  ) -> Result<()> {
    panic!("no seek")
  }

  fn term(&self) -> Result<Self::Value<'_>> {
    self.in_.term()
  }

  fn ord(&self) -> Result<i64> {
    self.in_.ord()
  }

  fn doc_freq(&mut self) -> Result<i32> {
    self.in_.doc_freq()
  }

  fn total_term_freq(&mut self) -> Result<i64> {
    self.in_.total_term_freq()
  }

  type PostingsEnum = TE::PostingsEnum;

  fn postings_with_flags(
    &mut self,
    reuse: Option<Self::PostingsEnum>,
    flags: i32,
  ) -> Result<Self::PostingsEnum> {
    self.in_.postings_with_flags(reuse, flags)
  }

  type ImpactsEnum = TE::ImpactsEnum;

  fn impacts(&mut self, flags: i32) -> Result<Self::ImpactsEnum> {
    self.in_.impacts(flags)
  }

  fn term_state(&mut self) -> Result<TermStateEnum> {
    self.in_.term_state()
  }
}

pub struct SimilarityImpl<S> {
  existing_similarity: S,
  scorer_called: Arc<AtomicBool>,
}
impl<S> SimilarityImpl<S>
where
  S: Similarity,
{
  fn new(existing_similarity: S, scorer_called: Arc<AtomicBool>) -> Self {
    Self {
      existing_similarity,
      scorer_called,
    }
  }
}

impl<S> Display for SimilarityImpl<S>
where
  S: Similarity,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "SimilarityImpl")
  }
}

impl<S> Similarity for SimilarityImpl<S>
where
  S: Similarity,
{
  fn compute_norm(&self, state: &FieldInvertState) -> Result<i64> {
    self.existing_similarity.compute_norm(state)
  }

  type SimScorer = BoxSimScorer;

  fn scorer(
    &self,
    boost: f32,
    collection_stats: &CollectionStatistics,
    term_stats: &[TermStatistics],
  ) -> Result<Self::SimScorer> {
    self
      .scorer_called
      .store(true, std::sync::atomic::Ordering::SeqCst);
    Ok(Box::new(self.existing_similarity.scorer(
      boost,
      collection_stats,
      term_stats,
    )?))
  }
}

use crate::core::search::weight::Weight;
