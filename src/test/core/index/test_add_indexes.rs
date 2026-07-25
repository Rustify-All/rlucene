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
use crate::core::document::document::Document;
use crate::core::document::field::Store;
use crate::core::document::field_type::FieldType;
use crate::core::document::int_point::IntPoint;
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::document::string_field::StringField;
use crate::core::index::composite_reader::CompositeReader;
use crate::core::index::concurrent_merge_scheduler::ConcurrentMergeScheduler;
use crate::core::index::directory_reader;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::{
  IndexWriter, MAX_DOCS, SOURCE, SOURCE_ADDINDEXES_READERS, set_max_docs,
};
use crate::core::index::index_writer_config::{IndexWriterConfig, OpenMode};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::log_merge_policy::LogMergePolicy;
use crate::core::index::merge_policy::{MergePolicy, MergePolicyEnum};
use crate::core::index::merge_scheduler::MergeSchedulerEnum;
use crate::core::index::no_merge_policy::NoMergePolicy;
use crate::core::index::postings_enum::NONE;
use crate::core::index::segment_infos::SegmentInfos;
use crate::core::index::segment_reader::DefaultLeafReader;
use crate::core::index::serial_merge_scheduler::SerialMergeScheduler;
use crate::core::index::slow_codec_reader_wrapper::SlowCodecReaderWrapper;
use crate::core::index::soft_deletes_directory_reader_wrapper::SoftDeletesDirectoryReaderWrapper;
use crate::core::index::standard_directory_reader::StandardDirectoryReader;
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::term::Term;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::phrase_query::PhraseQuery;
use crate::core::search::sort::Sort;
use crate::core::search::sort_field::{SortField, SortFieldType};
use crate::core::store::byte_buffers_directory::ByteBuffersDirectory;
use crate::core::store::directory::{DirEnum, Directory, DirectoryEnum2};
use crate::core::store::single_instance_lock_factory::SingleInstanceLockFactory;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::io_utils::IOUtils;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::index::all_deleted_filter_reader::AllDeletedFilterReader;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::index::test_add_indexes::{
  ConcurrentAddIndexesMergePolicy, CountingSerialMergeScheduler, PartialMergeScheduler,
};
use crate::test_framework::core::store::mock_directory_wrapper::MockDirectoryWrapper;
use crate::test_framework::core::util::lucene_test_case::{
  get_only_leaf_reader, is_night_mode, new_directory_shared, new_field, new_index_writer_config,
  new_index_writer_config_with_analyzer, new_log_merge_policy_with_cfs,
  new_log_merge_policy_with_merge_factor, new_log_merge_policy_with_merge_factor_cfs,
  new_string_field, new_text_field, random, random_from_seed,
};
use crate::test_framework::core::util::test_util::TestUtil;
use parking_lot::Mutex;
use rand::{Rng, RngExt};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

type AddIndexesDirectory =
  MockDirectoryWrapper<Arc<ByteBuffersDirectory<SingleInstanceLockFactory>>>;

#[allow(dead_code)] // for quick search
pub struct TestAddIndexes;

#[test]
fn test_simple_case() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let aux = new_directory_shared(&mut random)?;
  let aux2 = new_directory_shared(&mut random)?;
  let mut field_types = HashMap::new();

  let analyzer = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  conf.set_open_mode(OpenMode::Create);
  let writer = new_writer(dir.clone(), conf)?;
  add_docs(&mut random, &writer, 100, &mut field_types)?;
  assert_eq!(100, writer.get_doc_stats()?.max_doc);
  writer.close()?;
  drop(writer);
  TestUtil::check_index(&mut random, dir.clone())?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  conf.set_open_mode(OpenMode::Create);
  conf.set_merge_policy(new_log_merge_policy_with_cfs(&mut random, false)?);
  let writer = new_writer(aux.clone(), conf)?;
  add_docs(&mut random, &writer, 40, &mut field_types)?;
  assert_eq!(40, writer.get_doc_stats()?.max_doc);
  writer.close()?;
  drop(writer);

  let analyzer = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  conf.set_open_mode(OpenMode::Create);
  let writer = new_writer(aux2.clone(), conf)?;
  add_docs2(&mut random, &writer, 50, &mut field_types)?;
  assert_eq!(50, writer.get_doc_stats()?.max_doc);
  writer.close()?;
  drop(writer);

  let analyzer = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  conf.set_open_mode(OpenMode::Append);
  let writer = new_writer(dir.clone(), conf)?;
  assert_eq!(100, writer.get_doc_stats()?.max_doc);
  writer.add_indexes_from_directory(&[aux.clone(), aux2.clone()])?;
  assert_eq!(190, writer.get_doc_stats()?.max_doc);
  writer.close()?;
  TestUtil::check_index(&mut random, dir.clone())?;
  drop(writer);

  verify_num_docs(aux.clone(), 40)?;
  verify_num_docs(dir.clone(), 190)?;

  let aux3 = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  let writer = new_writer(aux3.clone(), conf)?;
  add_docs(&mut random, &writer, 40, &mut field_types)?;
  assert_eq!(40, writer.get_doc_stats()?.max_doc);
  writer.close()?;
  drop(writer);

  let analyzer = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  conf.set_open_mode(OpenMode::Append);
  let writer = new_writer(dir.clone(), conf)?;
  assert_eq!(190, writer.get_doc_stats()?.max_doc);
  writer.add_indexes_from_directory(std::slice::from_ref(&aux3))?;
  assert_eq!(230, writer.get_doc_stats()?.max_doc);
  writer.close()?;
  drop(writer);

  verify_num_docs(dir.clone(), 230)?;
  verify_term_docs(
    &mut random,
    dir.clone(),
    &Term::from_text("content", "aaa"),
    180,
  )?;
  verify_term_docs(
    &mut random,
    dir.clone(),
    &Term::from_text("content", "bbb"),
    50,
  )?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  conf.set_open_mode(OpenMode::Append);
  let writer = new_writer(dir.clone(), conf)?;
  writer.force_merge(1)?;
  writer.close()?;
  drop(writer);

  verify_num_docs(dir.clone(), 230)?;
  verify_term_docs(
    &mut random,
    dir.clone(),
    &Term::from_text("content", "aaa"),
    180,
  )?;
  verify_term_docs(
    &mut random,
    dir.clone(),
    &Term::from_text("content", "bbb"),
    50,
  )?;

  let aux4 = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  let writer = new_writer(aux4.clone(), conf)?;
  add_docs2(&mut random, &writer, 1, &mut field_types)?;
  writer.close()?;
  drop(writer);

  let analyzer = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  conf.set_open_mode(OpenMode::Append);
  let writer = new_writer(dir.clone(), conf)?;
  assert_eq!(230, writer.get_doc_stats()?.max_doc);
  writer.add_indexes_from_directory(std::slice::from_ref(&aux4))?;
  assert_eq!(231, writer.get_doc_stats()?.max_doc);
  writer.close()?;
  drop(writer);

  verify_num_docs(dir.clone(), 231)?;
  verify_term_docs(
    &mut random,
    dir.clone(),
    &Term::from_text("content", "bbb"),
    51,
  )?;
  Ok(())
}
#[test]
fn test_with_pending_deletes() -> Result<()> {
  // main directory
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  // auxiliary directory
  let aux = new_directory_shared(&mut random)?;
  let mut field_types = HashMap::new();

  set_up_dirs(&mut random, dir.clone(), aux.clone(), &mut field_types)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  conf.set_open_mode(OpenMode::Append);
  let writer = new_writer(dir.clone(), conf)?;
  writer.add_indexes_from_directory(std::slice::from_ref(&aux))?;

  // Adds 10 docs, then replaces them with another 10
  // docs, so 10 pending deletes:
  for i in 0..20 {
    let mut doc = Document::new();
    doc.add(StringField::from_string(
      "id",
      (i % 10).to_string(),
      Store::No,
    )?);
    doc.add(new_text_field(
      &mut random,
      "content",
      format!("bbb {i}"),
      Store::No,
      &mut field_types,
    )?);
    doc.add(IntPoint::new("doc", [i])?);
    doc.add(IntPoint::new("doc2d", [i, i])?);
    doc.add(NumericDocValuesField::new("dv", i as i64));
    writer.update_document_with_term(Term::from_text("id", (i % 10).to_string()), doc)?;
  }
  // Deletes one of the 10 added docs, leaving 9:
  let q = PhraseQuery::from_terms_no_slop("content", &["bbb", "14"])?;
  writer.delete_documents_with_queries(vec![q.into()])?;

  writer.force_merge(1)?;
  writer.commit()?;

  verify_num_docs(dir.clone(), 1039)?;
  verify_term_docs(
    &mut random,
    dir.clone(),
    &Term::from_text("content", "aaa"),
    1030,
  )?;
  verify_term_docs(
    &mut random,
    dir.clone(),
    &Term::from_text("content", "bbb"),
    9,
  )?;

  writer.close()?;
  Ok(())
}
#[test]
fn test_with_pending_deletes2() -> Result<()> {
  // main directory
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  // auxiliary directory
  let aux = new_directory_shared(&mut random)?;
  let mut field_types = HashMap::new();

  set_up_dirs(&mut random, dir.clone(), aux.clone(), &mut field_types)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  conf.set_open_mode(OpenMode::Append);
  let writer = new_writer(dir.clone(), conf)?;

  // Adds 10 docs, then replaces them with another 10
  // docs, so 10 pending deletes:
  for i in 0..20 {
    let mut doc = Document::new();
    doc.add(StringField::from_string(
      "id",
      (i % 10).to_string(),
      Store::No,
    )?);
    doc.add(new_text_field(
      &mut random,
      "content",
      format!("bbb {i}"),
      Store::No,
      &mut field_types,
    )?);
    doc.add(IntPoint::new("doc", [i])?);
    doc.add(IntPoint::new("doc2d", [i, i])?);
    doc.add(NumericDocValuesField::new("dv", i as i64));
    writer.update_document_with_term(Term::from_text("id", (i % 10).to_string()), doc)?;
  }

  writer.add_indexes_from_directory(std::slice::from_ref(&aux))?;

  // Deletes one of the 10 added docs, leaving 9:
  let q = PhraseQuery::from_terms_no_slop("content", &["bbb", "14"])?;
  writer.delete_documents_with_queries(vec![q.into()])?;

  writer.force_merge(1)?;
  writer.commit()?;

  verify_num_docs(dir.clone(), 1039)?;
  verify_term_docs(
    &mut random,
    dir.clone(),
    &Term::from_text("content", "aaa"),
    1030,
  )?;
  verify_term_docs(
    &mut random,
    dir.clone(),
    &Term::from_text("content", "bbb"),
    9,
  )?;

  writer.close()?;
  Ok(())
}
#[test]
fn test_with_pending_deletes3() -> Result<()> {
  // main directory
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  // auxiliary directory
  let aux = new_directory_shared(&mut random)?;
  let mut field_types = HashMap::new();

  set_up_dirs(&mut random, dir.clone(), aux.clone(), &mut field_types)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  conf.set_open_mode(OpenMode::Append);
  let writer = new_writer(dir.clone(), conf)?;

  // Adds 10 docs, then replaces them with another 10
  // docs, so 10 pending deletes:
  for i in 0..20 {
    let mut doc = Document::new();
    doc.add(StringField::from_string(
      "id",
      (i % 10).to_string(),
      Store::No,
    )?);
    doc.add(new_text_field(
      &mut random,
      "content",
      format!("bbb {i}"),
      Store::No,
      &mut field_types,
    )?);
    doc.add(IntPoint::new("doc", [i])?);
    doc.add(IntPoint::new("doc2d", [i, i])?);
    doc.add(NumericDocValuesField::new("dv", i as i64));
    writer.update_document_with_term(Term::from_text("id", (i % 10).to_string()), doc)?;
  }

  // Deletes one of the 10 added docs, leaving 9:
  let q = PhraseQuery::from_terms_no_slop("content", &["bbb", "14"])?;
  writer.delete_documents_with_queries(vec![q.into()])?;

  writer.add_indexes_from_directory(std::slice::from_ref(&aux))?;

  writer.force_merge(1)?;
  writer.commit()?;

  verify_num_docs(dir.clone(), 1039)?;
  verify_term_docs(
    &mut random,
    dir.clone(),
    &Term::from_text("content", "aaa"),
    1030,
  )?;
  verify_term_docs(
    &mut random,
    dir.clone(),
    &Term::from_text("content", "bbb"),
    9,
  )?;

  writer.close()?;
  Ok(())
}
#[test]
fn test_add_self() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let aux = new_directory_shared(&mut random)?;
  let mut field_types = HashMap::new();

  let analyzer = MockAnalyzer::new(&mut random);
  let conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  let writer = new_writer(dir.clone(), conf)?;
  add_docs(&mut random, &writer, 100, &mut field_types)?;
  assert_eq!(100, writer.get_doc_stats()?.max_doc);
  writer.close()?;
  drop(writer);

  let analyzer = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  conf.set_open_mode(OpenMode::Create);
  conf.set_max_buffered_docs(1000);
  conf.set_merge_policy(new_log_merge_policy_with_cfs(&mut random, false)?);
  let writer = new_writer(aux.clone(), conf)?;
  add_docs(&mut random, &writer, 40, &mut field_types)?;
  writer.close()?;
  drop(writer);

  let analyzer = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  conf.set_open_mode(OpenMode::Create);
  conf.set_max_buffered_docs(1000);
  conf.set_merge_policy(new_log_merge_policy_with_cfs(&mut random, false)?);
  let writer = new_writer(aux.clone(), conf)?;
  add_docs(&mut random, &writer, 100, &mut field_types)?;
  writer.close()?;
  drop(writer);

  let analyzer = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  conf.set_open_mode(OpenMode::Append);
  let writer2 = new_writer(dir.clone(), conf)?;

  let err = writer2.add_indexes_from_directory(&[aux.clone(), dir.clone()]);
  assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));

  assert_eq!(100, writer2.get_doc_stats()?.max_doc);
  writer2.close()?;
  drop(writer2);

  verify_num_docs(dir.clone(), 100)?;
  Ok(())
}
// in all the remaining tests, make the doc count of the oldest segment
// in dir large so that it is never merged in addIndexes()
// case 1: no tail segments
#[test]
fn test_no_tail_segments() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let aux = new_directory_shared(&mut random)?;
  let mut field_types = HashMap::new();

  set_up_dirs(&mut random, dir.clone(), aux.clone(), &mut field_types)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  conf.set_open_mode(OpenMode::Append);
  conf.set_max_buffered_docs(10);
  conf.set_merge_policy(new_log_merge_policy_with_merge_factor(&mut random, 4)?);
  let writer = new_writer(dir.clone(), conf)?;
  add_docs(&mut random, &writer, 10, &mut field_types)?;

  writer.add_indexes_from_directory(std::slice::from_ref(&aux))?;
  assert_eq!(1040, writer.get_doc_stats()?.max_doc);
  assert_eq!(1000, writer.max_doc(0));
  writer.close()?;
  drop(writer);

  verify_num_docs(dir.clone(), 1040)?;
  dir.close()?;
  aux.close()?;
  Ok(())
}

// Case 2: tail segments, invariants hold, no copy.
#[test]
fn test_no_copy_segments() -> Result<()> {
  // Main directory.
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  // Auxiliary directory.
  let aux = new_directory_shared(&mut random)?;
  let mut field_types = HashMap::new();

  set_up_dirs(&mut random, dir.clone(), aux.clone(), &mut field_types)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  conf.set_open_mode(OpenMode::Append);
  conf.set_max_buffered_docs(9);
  conf.set_merge_policy(new_log_merge_policy_with_merge_factor(&mut random, 4)?);
  let writer = new_writer(dir.clone(), conf)?;
  add_docs(&mut random, &writer, 2, &mut field_types)?;

  writer.add_indexes_from_directory(std::slice::from_ref(&aux))?;
  assert_eq!(1032, writer.get_doc_stats()?.max_doc);
  assert_eq!(1000, writer.max_doc(0));
  writer.close()?;

  // Make sure the index is correct.
  verify_num_docs(dir.clone(), 1032)?;
  dir.close()?;
  aux.close()?;
  Ok(())
}

// Case 3: tail segments, invariants hold, copy, invariants hold.
#[test]
fn test_no_merge_after_copy() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let aux = new_directory_shared(&mut random)?;
  let mut field_types = HashMap::new();

  set_up_dirs(&mut random, dir.clone(), aux.clone(), &mut field_types)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  conf.set_open_mode(OpenMode::Append);
  conf.set_max_buffered_docs(10);
  conf.set_merge_policy(new_log_merge_policy_with_merge_factor(&mut random, 4)?);
  let writer = new_writer(dir.clone(), conf)?;
  let aux_copy = TestUtil::ram_copy_of(&mut random, aux.as_ref())?;
  let dirs = [
    Arc::new(DirectoryEnum2::A(aux.clone())),
    Arc::new(DirectoryEnum2::B(MockDirectoryWrapper::new(
      &mut random,
      aux_copy,
    ))),
  ];
  writer.add_indexes_from_directory(&dirs)?;
  assert_eq!(1060, writer.get_doc_stats()?.max_doc);
  assert_eq!(1000, writer.max_doc(0));
  writer.close()?;

  verify_num_docs(dir.clone(), 1060)?;
  dir.close()?;
  aux.close()?;
  Ok(())
}
#[test]
fn test_merge_after_copy() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let aux = new_directory_shared(&mut random)?;
  let mut field_types = HashMap::new();

  set_up_dirs_with_id(
    &mut random,
    dir.clone(),
    aux.clone(),
    true,
    &mut field_types,
  )?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut dont_merge_config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  dont_merge_config.set_merge_policy(NoMergePolicy::default());
  let writer = IndexWriter::new(aux.clone(), dont_merge_config)?;
  for i in 0..20 {
    writer.delete_documents_with_terms(vec![Term::from_text("id", i.to_string())])?;
  }
  writer.close()?;
  drop(writer);

  let reader = directory_reader::open(aux.clone())?;
  assert_eq!(10, reader.num_docs()?);
  reader.close()?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  conf.set_open_mode(OpenMode::Append);
  conf.set_max_buffered_docs(4);
  conf.set_merge_policy(new_log_merge_policy_with_merge_factor(&mut random, 4)?);
  let writer = new_writer(dir.clone(), conf)?;

  if cfg!(feature = "test_log_verbose") {
    println!("\nTEST: now addIndexes");
  }
  let aux_copy = TestUtil::ram_copy_of(&mut random, aux.as_ref())?;
  let dirs = [
    Arc::new(DirectoryEnum2::A(aux.clone())),
    Arc::new(DirectoryEnum2::B(MockDirectoryWrapper::new(
      &mut random,
      aux_copy,
    ))),
  ];
  writer.add_indexes_from_directory(&dirs)?;
  assert_eq!(1020, writer.get_doc_stats()?.max_doc);
  assert_eq!(1000, writer.max_doc(0));
  writer.close()?;
  dir.close()?;
  aux.close()?;
  Ok(())
}

#[test]
fn test_more_merges() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let aux = new_directory_shared(&mut random)?;
  let aux2 = new_directory_shared(&mut random)?;
  let mut field_types = HashMap::new();

  set_up_dirs_with_id(
    &mut random,
    dir.clone(),
    aux.clone(),
    true,
    &mut field_types,
  )?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  conf.set_open_mode(OpenMode::Create);
  conf.set_max_buffered_docs(100);
  conf.set_merge_policy(new_log_merge_policy_with_merge_factor(&mut random, 10)?);
  let writer = new_writer(aux2.clone(), conf)?;
  writer.add_indexes_from_directory(std::slice::from_ref(&aux))?;
  assert_eq!(30, writer.get_doc_stats()?.max_doc);
  assert_eq!(3, writer.get_segment_count());
  writer.close()?;
  drop(writer);

  let analyzer = MockAnalyzer::new(&mut random);
  let mut dont_merge_config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  dont_merge_config.set_merge_policy(NoMergePolicy::default());
  let writer = IndexWriter::new(aux.clone(), dont_merge_config)?;
  for i in 0..27 {
    writer.delete_documents_with_terms(vec![Term::from_text("id", i.to_string())])?;
  }
  writer.close()?;
  drop(writer);

  let reader = directory_reader::open(aux.clone())?;
  assert_eq!(3, reader.num_docs()?);
  reader.close()?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut dont_merge_config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  dont_merge_config.set_merge_policy(NoMergePolicy::default());
  let writer = IndexWriter::new(aux2.clone(), dont_merge_config)?;
  for i in 0..8 {
    writer.delete_documents_with_terms(vec![Term::from_text("id", i.to_string())])?;
  }
  writer.close()?;
  drop(writer);

  let reader = directory_reader::open(aux2.clone())?;
  assert_eq!(22, reader.num_docs()?);
  reader.close()?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  conf.set_open_mode(OpenMode::Append);
  conf.set_max_buffered_docs(6);
  conf.set_merge_policy(new_log_merge_policy_with_merge_factor(&mut random, 4)?);
  let writer = new_writer(dir.clone(), conf)?;

  writer.add_indexes_from_directory(&[aux.clone(), aux2.clone()])?;
  assert_eq!(1040, writer.get_doc_stats()?.max_doc);
  assert_eq!(1000, writer.max_doc(0));
  writer.close()?;
  Ok(())
}
fn new_writer<D>(dir: Arc<D>, mut conf: IndexWriterConfig<D>) -> Result<Arc<IndexWriter<D>>>
where
  D: Directory + 'static,
{
  conf.set_merge_policy(LogMergePolicy::log_doc());
  IndexWriter::new(dir, conf)
}
fn add_docs<D, R>(
  random: &mut R,
  writer: &IndexWriter<D>,
  num_docs: i32,
  field_types: &mut HashMap<String, FieldType>,
) -> Result<()>
where
  D: Directory + 'static,
  R: Rng + ?Sized,
{
  for i in 0..num_docs {
    let mut doc = Document::new();
    doc.add(new_text_field(
      random,
      "content",
      "aaa",
      Store::No,
      field_types,
    )?);
    doc.add(IntPoint::new("doc", [i])?);
    doc.add(IntPoint::new("doc2d", [i, i])?);
    doc.add(NumericDocValuesField::new("dv", i as i64));
    writer.add_document(doc)?;
  }
  Ok(())
}

fn add_docs2<D, R>(
  random: &mut R,
  writer: &IndexWriter<D>,
  num_docs: i32,
  field_types: &mut HashMap<String, FieldType>,
) -> Result<()>
where
  D: Directory + 'static,
  R: Rng + ?Sized,
{
  for i in 0..num_docs {
    let mut doc = Document::new();
    doc.add(new_text_field(
      random,
      "content",
      "bbb",
      Store::No,
      field_types,
    )?);
    doc.add(IntPoint::new("doc", [i])?);
    doc.add(IntPoint::new("doc2d", [i, i])?);
    doc.add(NumericDocValuesField::new("dv", i as i64));
    writer.add_document(doc)?;
  }
  Ok(())
}

fn verify_num_docs<D>(dir: Arc<D>, num_docs: i32) -> Result<()>
where
  D: Directory + 'static,
{
  let reader = directory_reader::open(dir)?;
  assert_eq!(num_docs, reader.max_doc()?);
  assert_eq!(num_docs, reader.num_docs()?);
  reader.close()?;
  Ok(())
}

fn verify_term_docs<R, D>(random: &mut R, dir: Arc<D>, term: &Term, num_docs: i32) -> Result<()>
where
  D: Directory + 'static,
  R: Rng + ?Sized,
{
  let reader = directory_reader::open(dir)?;
  let mut postings_enum = TestUtil::docs_with_reader(
    random,
    &reader,
    term.field(),
    term.bytes(),
    None,
    NONE as i32,
  )?
  .unwrap();

  let mut count = 0;
  while postings_enum.next_doc()? != NO_MORE_DOCS {
    count += 1;
  }

  assert_eq!(num_docs, count);
  reader.close()?;
  Ok(())
}
fn add_docs_with_id<R, D>(
  random: &mut R,
  writer: &IndexWriter<D>,
  num_docs: i32,
  doc_start: i32,
  field_to_type: &mut HashMap<String, FieldType>,
) -> Result<()>
where
  R: Rng + ?Sized,
  D: Directory + 'static,
{
  for i in 0..num_docs {
    let mut doc = Document::new();
    doc.add(new_text_field(
      random,
      "content",
      "aaa",
      Store::No,
      field_to_type,
    )?);
    doc.add(new_text_field(
      random,
      "id",
      (doc_start + i).to_string(),
      Store::Yes,
      field_to_type,
    )?);
    doc.add(IntPoint::new("doc", vec![i])?);
    doc.add(IntPoint::new("doc2d", vec![i, i])?);
    doc.add(NumericDocValuesField::new("dv", i as i64));
    writer.add_document(doc)?;
  }
  Ok(())
}

fn set_up_dirs<R, D1, D2>(
  random: &mut R,
  dir: Arc<D1>,
  aux: Arc<D2>,
  field_types: &mut HashMap<String, FieldType>,
) -> Result<()>
where
  R: Rng + ?Sized,
  D1: Directory + 'static,
  D2: Directory + 'static,
{
  set_up_dirs_with_id(random, dir, aux, false, field_types)
}

fn set_up_dirs_with_id<R, D1, D2>(
  random: &mut R,
  dir: Arc<D1>,
  aux: Arc<D2>,
  with_id: bool,
  field_types: &mut HashMap<String, FieldType>,
) -> Result<()>
where
  R: Rng + ?Sized,
  D1: Directory + 'static,
  D2: Directory + 'static,
{
  let analyzer = MockAnalyzer::new(random);
  let mut conf = new_index_writer_config_with_analyzer(random, analyzer)?;
  conf.set_open_mode(OpenMode::Create);
  conf.set_max_buffered_docs(1000);
  let writer = new_writer(dir.clone(), conf)?;

  if with_id {
    add_docs_with_id(random, &writer, 1000, 0, field_types)?;
  } else {
    add_docs(random, &writer, 1000, field_types)?;
  }
  assert_eq!(1000, writer.get_doc_stats()?.max_doc);
  assert_eq!(1, writer.get_segment_count());
  writer.close()?;
  drop(writer);

  let analyzer = MockAnalyzer::new(random);
  let mut conf = new_index_writer_config_with_analyzer(random, analyzer)?;
  conf.set_open_mode(OpenMode::Create);
  conf.set_max_buffered_docs(1000);
  conf.set_merge_policy(new_log_merge_policy_with_merge_factor_cfs(
    random, false, 10,
  )?);

  let mut writer = new_writer(aux.clone(), conf)?;

  for i in 0..3 {
    if with_id {
      add_docs_with_id(random, &writer, 10, 10 * i, field_types)?;
    } else {
      add_docs(random, &writer, 10, field_types)?;
    }
    writer.close()?;
    drop(writer);

    let analyzer = MockAnalyzer::new(random);
    let mut conf = new_index_writer_config_with_analyzer(random, analyzer)?;
    conf.set_open_mode(OpenMode::Append);
    conf.set_max_buffered_docs(1000);
    conf.set_merge_policy(new_log_merge_policy_with_merge_factor_cfs(
      random, false, 10,
    )?);
    writer = new_writer(aux.clone(), conf)?;
  }

  assert_eq!(30, writer.get_doc_stats()?.max_doc);
  assert_eq!(3, writer.get_segment_count());
  writer.close()?;
  Ok(())
}
#[test]
fn test_hang_on_close() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mut lmp = LogMergePolicy::log_bytes_size();
  MergePolicy::<DirEnum>::get_base_mut(&mut lmp).set_no_cfs_ratio(0.0)?;
  lmp.set_merge_factor(100)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  iwc.set_max_buffered_docs(5);
  iwc.set_merge_policy(lmp);
  let writer = IndexWriter::new(dir.clone(), iwc)?;

  let mut doc = Document::new();
  let mut custom_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_STORED)?;
  let mut field_types = HashMap::new();
  custom_type.set_store_term_vectors(true)?;
  custom_type.set_store_term_vector_positions(true)?;
  custom_type.set_store_term_vector_offsets(true)?;
  doc.add(new_field(
    &mut random,
    "content",
    "aaa bbb ccc ddd eee fff ggg hhh iii",
    &custom_type,
    &mut field_types,
  )?);
  for _ in 0..60 {
    writer.add_document(doc.clone())?;
  }

  let mut doc2 = Document::new();
  let mut custom_type2 = FieldType::new();
  custom_type2.set_stored(true)?;
  doc2.add(new_field(
    &mut random,
    "content",
    "aaa bbb ccc ddd eee fff ggg hhh iii",
    &custom_type2,
    &mut field_types,
  )?);
  doc2.add(new_field(
    &mut random,
    "content",
    "aaa bbb ccc ddd eee fff ggg hhh iii",
    &custom_type2,
    &mut field_types,
  )?);
  doc2.add(new_field(
    &mut random,
    "content",
    "aaa bbb ccc ddd eee fff ggg hhh iii",
    &custom_type2,
    &mut field_types,
  )?);
  doc2.add(new_field(
    &mut random,
    "content",
    "aaa bbb ccc ddd eee fff ggg hhh iii",
    &custom_type2,
    &mut field_types,
  )?);

  for _ in 0..10 {
    writer.add_document(doc2.clone())?;
  }
  writer.close()?;
  drop(writer);

  let dir2 = new_directory_shared(&mut random)?;
  let mut lmp = LogMergePolicy::log_bytes_size();
  lmp.set_min_merge_mb(0.0001);
  MergePolicy::<DirEnum>::get_base_mut(&mut lmp).set_no_cfs_ratio(0.0)?;
  lmp.set_merge_factor(4)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  iwc.set_merge_scheduler(SerialMergeScheduler::new());
  iwc.set_merge_policy(lmp);
  let writer = IndexWriter::new(dir2.clone(), iwc)?;
  writer.add_indexes_from_directory(std::slice::from_ref(&dir))?;
  writer.close()?;
  Ok(())
}
fn add_doc<D, R>(
  random: &mut R,
  writer: &IndexWriter<D>,
  field_types: &mut HashMap<String, FieldType>,
) -> Result<()>
where
  D: Directory + 'static,
  R: Rng + ?Sized,
{
  let mut doc = Document::new();
  doc.add(new_text_field(
    random,
    "content",
    "aaa",
    Store::No,
    field_types,
  )?);
  writer.add_document(doc)?;
  Ok(())
}

struct AddIndexesWithReadersSetup {
  dir: Arc<AddIndexesDirectory>,
  dest_dir: Arc<AddIndexesDirectory>,
  dest_writer: Arc<IndexWriter<AddIndexesDirectory>>,
  readers: Vec<StandardDirectoryReader<AddIndexesDirectory>>,
}

impl AddIndexesWithReadersSetup {
  const ADDED_DOCS_PER_READER: i32 = 15;
  const INIT_DOCS: i32 = 25;
  const NUM_READERS: usize = 15;

  fn new<R, MS, MP>(random: &mut R, merge_scheduler: MS, merge_policy: MP) -> Result<Self>
  where
    R: Rng + ?Sized,
    MS: Into<MergeSchedulerEnum>,
    MP: Into<MergePolicyEnum<AddIndexesDirectory>>,
  {
    let dir: Arc<AddIndexesDirectory> = Arc::new(MockDirectoryWrapper::new(
      &mut *random,
      Arc::new(ByteBuffersDirectory::new()),
    ));
    let analyzer = MockAnalyzer::new(&mut *random);
    let mut iwc = new_index_writer_config_with_analyzer(&mut *random, analyzer)?;
    iwc.set_max_buffered_docs(2);
    let writer = IndexWriter::new(dir.clone(), iwc)?;
    let mut field_types = HashMap::new();
    for _ in 0..Self::ADDED_DOCS_PER_READER {
      add_doc(random, &writer, &mut field_types)?;
    }
    writer.close()?;

    let dest_dir: Arc<AddIndexesDirectory> = Arc::new(MockDirectoryWrapper::new(
      &mut *random,
      Arc::new(ByteBuffersDirectory::new()),
    ));
    let analyzer = MockAnalyzer::new(&mut *random);
    let mut iwc = new_index_writer_config_with_analyzer(&mut *random, analyzer)?;
    iwc.set_merge_policy(merge_policy);
    iwc.set_merge_scheduler(merge_scheduler);
    let dest_writer = IndexWriter::new(dest_dir.clone(), iwc)?;
    for _ in 0..Self::INIT_DOCS {
      add_doc(random, &dest_writer, &mut field_types)?;
    }
    dest_writer.commit()?;

    let mut readers = Vec::with_capacity(Self::NUM_READERS);
    for _ in 0..Self::NUM_READERS {
      readers.push(directory_reader::open(dir.clone())?);
    }

    Ok(Self {
      dir,
      dest_dir,
      dest_writer,
      readers,
    })
  }

  fn close_all(&self) -> Result<()> {
    self.dest_writer.close()?;
    for reader in &self.readers {
      reader.close()?;
    }
    self.dest_dir.close()?;
    self.dir.close()
  }
}

#[test]
fn test_add_indexes_with_concurrent_merges() -> Result<()> {
  let mut random = random();
  let c = AddIndexesWithReadersSetup::new(
    &mut random,
    ConcurrentMergeScheduler::new(),
    ConcurrentAddIndexesMergePolicy::default(),
  )?;
  TestUtil::add_indexes_slowly(&c.dest_writer, &c.readers)?;
  c.dest_writer.commit()?;
  let reader = directory_reader::open(c.dest_dir.clone())?;
  assert_eq!(
    AddIndexesWithReadersSetup::INIT_DOCS
      + AddIndexesWithReadersSetup::NUM_READERS as i32
        * AddIndexesWithReadersSetup::ADDED_DOCS_PER_READER,
    reader.num_docs()?
  );
  reader.close()?;
  c.close_all()
}

#[test]
fn test_add_indexes_with_partial_merge_failures() -> Result<()> {
  let mut random = random();
  let c = AddIndexesWithReadersSetup::new(
    &mut random,
    PartialMergeScheduler::new(2),
    ConcurrentAddIndexesMergePolicy::default(),
  )?;
  let error = TestUtil::add_indexes_slowly(&c.dest_writer, &c.readers)
    .expect_err("partially failed addIndexes merges must fail");
  assert!(
    matches!(error, LuceneError::IllegalState(_)),
    "unexpected error: {error}"
  );
  c.dest_writer.commit()?;

  // Verify no docs got added and all interim files from successful merges have been deleted.
  let reader = directory_reader::open(c.dest_dir.clone())?;
  assert_eq!(AddIndexesWithReadersSetup::INIT_DOCS, reader.num_docs()?);
  reader.close()?;
  c.close_all()
}

#[test]
fn test_add_indexes_with_null_merge_spec() -> Result<()> {
  let mut random = random();
  let c = AddIndexesWithReadersSetup::new(
    &mut random,
    ConcurrentMergeScheduler::new(),
    ConcurrentAddIndexesMergePolicy::null_merge_specification(),
  )?;
  TestUtil::add_indexes_slowly(&c.dest_writer, &c.readers)?;
  c.dest_writer.commit()?;
  let reader = directory_reader::open(c.dest_dir.clone())?;
  assert_eq!(AddIndexesWithReadersSetup::INIT_DOCS, reader.num_docs()?);
  reader.close()?;
  c.close_all()
}

#[test]
fn test_add_indexes_with_empty_merge_spec() -> Result<()> {
  let mut random = random();
  let c = AddIndexesWithReadersSetup::new(
    &mut random,
    ConcurrentMergeScheduler::new(),
    ConcurrentAddIndexesMergePolicy::empty_merge_specification(),
  )?;
  TestUtil::add_indexes_slowly(&c.dest_writer, &c.readers)?;
  c.dest_writer.commit()?;
  let reader = directory_reader::open(c.dest_dir.clone())?;
  assert_eq!(AddIndexesWithReadersSetup::INIT_DOCS, reader.num_docs()?);
  reader.close()?;
  c.close_all()
}

#[test]
fn test_add_indexes_with_empty_readers() -> Result<()> {
  let mut random = random();
  let dest_dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  iwc.set_merge_policy(ConcurrentAddIndexesMergePolicy::default());
  let merge_scheduler = CountingSerialMergeScheduler::default();
  iwc.set_merge_scheduler(merge_scheduler.clone());
  let dest_writer = IndexWriter::new(dest_dir.clone(), iwc)?;
  const INITIAL_DOCS: i32 = 15;
  let mut field_types = HashMap::new();
  for _ in 0..INITIAL_DOCS {
    add_doc(&mut random, &dest_writer, &mut field_types)?;
  }
  dest_writer.commit()?;

  // Create empty readers.
  let dir: Arc<AddIndexesDirectory> = Arc::new(MockDirectoryWrapper::new(
    &mut random,
    Arc::new(ByteBuffersDirectory::new()),
  ));
  let analyzer = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let writer = IndexWriter::new(dir.clone(), iwc)?;
  writer.close()?;
  const NUM_READERS: usize = 20;
  let mut readers = Vec::with_capacity(NUM_READERS);
  for _ in 0..NUM_READERS {
    readers.push(directory_reader::open(dir.clone())?);
  }

  TestUtil::add_indexes_slowly(&dest_writer, &readers)?;
  dest_writer.commit()?;

  // Verify no docs were added.
  let reader = directory_reader::open(dest_dir.clone())?;
  assert_eq!(INITIAL_DOCS, reader.num_docs()?);
  reader.close()?;
  // Verify no merges were triggered.
  assert_eq!(0, merge_scheduler.add_indexes_merges());

  dest_writer.close()?;
  for reader in &readers {
    reader.close()?;
  }
  dest_dir.close()?;
  dir.close()
}

#[test]
fn test_cascading_merges_triggered() -> Result<()> {
  let mut random = random();
  let merge_scheduler = CountingSerialMergeScheduler::default();
  let c = AddIndexesWithReadersSetup::new(
    &mut random,
    merge_scheduler.clone(),
    ConcurrentAddIndexesMergePolicy::default(),
  )?;
  TestUtil::add_indexes_slowly(&c.dest_writer, &c.readers)?;
  assert!(merge_scheduler.explicit_merges() > 0);
  c.close_all()
}

#[test]
fn test_add_indexes_hitting_max_docs_limit() -> Result<()> {
  const WRITER_MAX_DOCS: i32 = 15;
  set_max_docs(WRITER_MAX_DOCS)?;
  let result = (|| -> Result<()> {
    let mut random = random();

    // Create destination writer.
    let dest_dir = new_directory_shared(&mut random)?;
    let analyzer = MockAnalyzer::new(&mut random);
    let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
    iwc.set_merge_policy(ConcurrentAddIndexesMergePolicy::default());
    iwc.set_merge_scheduler(CountingSerialMergeScheduler::default());
    let dest_writer = IndexWriter::new(dest_dir.clone(), iwc)?;
    let mut field_types = HashMap::new();
    for _ in 0..WRITER_MAX_DOCS {
      add_doc(&mut random, &dest_writer, &mut field_types)?;
    }
    dest_writer.commit()?;

    // Create readers to add.
    let dir: Arc<AddIndexesDirectory> = Arc::new(MockDirectoryWrapper::new(
      &mut random,
      Arc::new(ByteBuffersDirectory::new()),
    ));
    let analyzer = MockAnalyzer::new(&mut random);
    let iwc = IndexWriterConfig::with_analyzer(analyzer)?;
    let writer = IndexWriter::new(dir.clone(), iwc)?;
    for _ in 0..10 {
      add_doc(&mut random, &writer, &mut field_types)?;
    }
    writer.close()?;
    const NUM_READERS: usize = 20;
    let mut readers = Vec::with_capacity(NUM_READERS);
    for _ in 0..NUM_READERS {
      readers.push(directory_reader::open(dir.clone())?);
    }

    let error = TestUtil::add_indexes_slowly(&dest_writer, &readers)
      .expect_err("addIndexes must reject documents beyond the configured maximum");
    assert!(
      matches!(&error, LuceneError::IllegalArgument(message) if message.message.contains(&format!(
        "number of documents in the index cannot exceed {WRITER_MAX_DOCS}"
      ))),
      "unexpected error: {error}"
    );

    // Verify no docs were added.
    dest_writer.commit()?;
    let reader = directory_reader::open(dest_dir.clone())?;
    assert_eq!(WRITER_MAX_DOCS, reader.num_docs()?);
    reader.close()?;

    dest_writer.close()?;
    for reader in &readers {
      reader.close()?;
    }
    dest_dir.close()?;
    dir.close()
  })();
  let restore_result = set_max_docs(MAX_DOCS);
  IOUtils::use_or_suppress_result(result, restore_result)
}

struct RunAddIndexesThreads {
  dir: Arc<AddIndexesDirectory>,
  dir2: Arc<AddIndexesDirectory>,
  writer2: Arc<IndexWriter<AddIndexesDirectory>>,
  failures: Arc<Mutex<Vec<String>>>,
  did_close: Arc<AtomicBool>,
  readers: Arc<Vec<StandardDirectoryReader<AddIndexesDirectory>>>,
  field_types: HashMap<String, FieldType>,
  num_copy: usize,
  threads: Vec<JoinHandle<()>>,
}

impl RunAddIndexesThreads {
  const NUM_INIT_DOCS: i32 = 17;

  fn new<R>(random: &mut R, num_copy: usize) -> Result<Self>
  where
    R: Rng + ?Sized,
  {
    let dir: Arc<AddIndexesDirectory> = Arc::new(MockDirectoryWrapper::new(
      &mut *random,
      Arc::new(ByteBuffersDirectory::new()),
    ));
    let analyzer = MockAnalyzer::new(&mut *random);
    let mut iwc = new_index_writer_config_with_analyzer(&mut *random, analyzer)?;
    iwc.set_max_buffered_docs(2);
    let writer = IndexWriter::new(dir.clone(), iwc)?;
    let mut field_types = HashMap::new();
    for _ in 0..Self::NUM_INIT_DOCS {
      add_doc(random, &writer, &mut field_types)?;
    }
    writer.close()?;

    let dir2: Arc<AddIndexesDirectory> = Arc::new(MockDirectoryWrapper::new(
      &mut *random,
      Arc::new(ByteBuffersDirectory::new()),
    ));
    let analyzer = MockAnalyzer::new(&mut *random);
    let mut iwc = new_index_writer_config_with_analyzer(&mut *random, analyzer)?;
    iwc.set_merge_policy(ConcurrentAddIndexesMergePolicy::default());
    let writer2 = IndexWriter::new(dir2.clone(), iwc)?;
    writer2.commit()?;

    let mut readers = Vec::with_capacity(num_copy);
    for _ in 0..num_copy {
      readers.push(directory_reader::open(dir.clone())?);
    }

    Ok(Self {
      dir,
      dir2,
      writer2,
      failures: Arc::new(Mutex::new(Vec::new())),
      did_close: Arc::new(AtomicBool::new(false)),
      readers: Arc::new(readers),
      field_types,
      num_copy,
      threads: Vec::new(),
    })
  }

  fn join_threads(&mut self) -> Result<()> {
    for thread in self.threads.drain(..) {
      thread.join().map_err(|payload| {
        LuceneError::tragedy_from_panic("panic in addIndexes thread", payload.as_ref())
      })?;
    }
    Ok(())
  }

  fn launch_threads<B, R>(&mut self, random: &mut R, num_iter: i32)
  where
    B: RunAddIndexesThreadBody,
    R: Rng + ?Sized,
  {
    let num_threads = if is_night_mode() { 5 } else { 2 };
    for _ in 0..num_threads {
      let seed = random.random();
      let dir = self.dir.clone();
      let writer2 = self.writer2.clone();
      let readers = self.readers.clone();
      let failures = self.failures.clone();
      let did_close = self.did_close.clone();
      let num_copy = self.num_copy;
      self.threads.push(thread::spawn(move || {
        let result = (|| -> Result<()> {
          let mut random = random_from_seed(seed);
          let mut dirs = Vec::with_capacity(num_copy);
          for _ in 0..num_copy {
            let dir_copy = TestUtil::ram_copy_of(&mut random, dir.as_ref())?;
            dirs.push(Arc::new(MockDirectoryWrapper::new(&mut random, dir_copy)));
          }

          let mut j = 0;
          loop {
            if num_iter > 0 && j == num_iter {
              break;
            }
            B::do_body(j, &writer2, readers.as_ref(), &dirs)?;
            j += 1;
          }
          Ok(())
        })();
        if let Err(error) = result {
          B::handle(error, &did_close, &failures);
        }
      }));
    }
  }

  fn close(&self, do_wait: bool) -> Result<()> {
    self.did_close.store(true, Ordering::SeqCst);
    if do_wait {
      self.writer2.close()
    } else {
      self.writer2.rollback()
    }
  }

  fn close_dir(&self) -> Result<()> {
    for reader in self.readers.iter() {
      reader.close()?;
    }
    self.dir2.close()
  }
}

trait RunAddIndexesThreadBody: Send + 'static {
  fn do_body(
    j: i32,
    writer2: &IndexWriter<AddIndexesDirectory>,
    readers: &[StandardDirectoryReader<AddIndexesDirectory>],
    dirs: &[Arc<AddIndexesDirectory>],
  ) -> Result<()>;

  fn handle(error: LuceneError, did_close: &AtomicBool, failures: &Mutex<Vec<String>>);
}

struct CommitAndAddIndexes {
  base: RunAddIndexesThreads,
}

impl CommitAndAddIndexes {
  fn new<R>(random: &mut R, num_copy: usize) -> Result<Self>
  where
    R: Rng + ?Sized,
  {
    Ok(Self {
      base: RunAddIndexesThreads::new(random, num_copy)?,
    })
  }

  fn launch_threads<R>(&mut self, random: &mut R, num_iter: i32)
  where
    R: Rng + ?Sized,
  {
    self
      .base
      .launch_threads::<CommitAndAddIndexes, R>(random, num_iter);
  }
}

impl RunAddIndexesThreadBody for CommitAndAddIndexes {
  fn do_body(
    j: i32,
    writer2: &IndexWriter<AddIndexesDirectory>,
    readers: &[StandardDirectoryReader<AddIndexesDirectory>],
    dirs: &[Arc<AddIndexesDirectory>],
  ) -> Result<()> {
    match j % 5 {
      0 => {
        writer2.add_indexes_from_directory(dirs)?;
        if let Err(error) = writer2.force_merge(1)
          && !matches!(error, LuceneError::MergeAborted(_))
        {
          return Err(error);
        }
      },
      1 => {
        writer2.add_indexes_from_directory(dirs)?;
      },
      2 => {
        TestUtil::add_indexes_slowly(writer2, readers)?;
      },
      3 => {
        writer2.add_indexes_from_directory(dirs)?;
        writer2.maybe_merge()?;
      },
      4 => {
        writer2.commit()?;
      },
      _ => unreachable!(),
    }
    Ok(())
  }

  fn handle(error: LuceneError, _did_close: &AtomicBool, failures: &Mutex<Vec<String>>) {
    if cfg!(feature = "test_log_verbose") {
      println!("{error:?}");
    }
    failures.lock().push(error.to_string());
  }
}

#[test]
fn test_add_indexes_with_threads() -> Result<()> {
  let mut random = random();
  let num_iter = if is_night_mode() { 15 } else { 5 };
  const NUM_COPY: usize = 3;
  let mut c = CommitAndAddIndexes::new(&mut random, NUM_COPY)?;
  c.launch_threads(&mut random, num_iter);

  for _ in 0..100 {
    add_doc(&mut random, &c.base.writer2, &mut c.base.field_types)?;
  }

  let thread_count = c.base.threads.len();
  c.base.join_threads()?;

  let expected_num_docs = 100
    + NUM_COPY as i32
      * (4 * num_iter / 5)
      * thread_count as i32
      * RunAddIndexesThreads::NUM_INIT_DOCS;
  assert_eq!(
    expected_num_docs,
    c.base.writer2.get_doc_stats()?.num_docs,
    "expected num docs don't match - failures: {:?}",
    c.base.failures.lock()
  );

  c.base.close(true)?;
  assert!(
    c.base.failures.lock().is_empty(),
    "found unexpected failures: {:?}",
    c.base.failures.lock()
  );

  let reader = directory_reader::open(c.base.dir2.clone())?;
  assert_eq!(expected_num_docs, reader.num_docs()?);
  reader.close()?;
  c.base.close_dir()?;
  Ok(())
}

struct CommitAndAddIndexes2 {
  base: RunAddIndexesThreads,
}

impl CommitAndAddIndexes2 {
  fn new<R>(random: &mut R, num_copy: usize) -> Result<Self>
  where
    R: Rng + ?Sized,
  {
    Ok(Self {
      base: RunAddIndexesThreads::new(random, num_copy)?,
    })
  }

  fn launch_threads<R>(&mut self, random: &mut R, num_iter: i32)
  where
    R: Rng + ?Sized,
  {
    self
      .base
      .launch_threads::<CommitAndAddIndexes2, R>(random, num_iter);
  }
}

impl RunAddIndexesThreadBody for CommitAndAddIndexes2 {
  fn do_body(
    j: i32,
    writer2: &IndexWriter<AddIndexesDirectory>,
    readers: &[StandardDirectoryReader<AddIndexesDirectory>],
    dirs: &[Arc<AddIndexesDirectory>],
  ) -> Result<()> {
    CommitAndAddIndexes::do_body(j, writer2, readers, dirs)
  }

  fn handle(error: LuceneError, _did_close: &AtomicBool, failures: &Mutex<Vec<String>>) {
    if !matches!(error, LuceneError::AlreadyClosed(_)) {
      if cfg!(feature = "test_log_verbose") {
        println!("{error:?}");
      }
      failures.lock().push(error.to_string());
    }
  }
}

#[test]
fn test_add_indexes_with_close() -> Result<()> {
  let mut random = random();
  const NUM_COPY: usize = 3;
  let mut c = CommitAndAddIndexes2::new(&mut random, NUM_COPY)?;
  c.launch_threads(&mut random, -1);

  c.base.close(true)?;
  c.base.join_threads()?;
  c.base.close_dir()?;
  assert!(
    c.base.failures.lock().is_empty(),
    "unexpected failures: {:?}",
    c.base.failures.lock()
  );
  Ok(())
}

struct CommitAndAddIndexes3 {
  base: RunAddIndexesThreads,
}

impl CommitAndAddIndexes3 {
  fn new<R>(random: &mut R, num_copy: usize) -> Result<Self>
  where
    R: Rng + ?Sized,
  {
    Ok(Self {
      base: RunAddIndexesThreads::new(random, num_copy)?,
    })
  }

  fn launch_threads<R>(&mut self, random: &mut R, num_iter: i32)
  where
    R: Rng + ?Sized,
  {
    self
      .base
      .launch_threads::<CommitAndAddIndexes3, R>(random, num_iter);
  }
}

impl RunAddIndexesThreadBody for CommitAndAddIndexes3 {
  fn do_body(
    j: i32,
    writer2: &IndexWriter<AddIndexesDirectory>,
    readers: &[StandardDirectoryReader<AddIndexesDirectory>],
    dirs: &[Arc<AddIndexesDirectory>],
  ) -> Result<()> {
    match j % 5 {
      0 => {
        writer2.add_indexes_from_directory(dirs)?;
        writer2.force_merge(1)?;
      },
      1 => {
        writer2.add_indexes_from_directory(dirs)?;
      },
      2 => {
        TestUtil::add_indexes_slowly(writer2, readers)?;
      },
      3 => {
        writer2.force_merge(1)?;
      },
      4 => {
        writer2.commit()?;
      },
      _ => unreachable!(),
    }
    Ok(())
  }

  fn handle(error: LuceneError, did_close: &AtomicBool, failures: &Mutex<Vec<String>>) {
    let did_close = did_close.load(Ordering::SeqCst);
    let report = match &error {
      LuceneError::AlreadyClosed(_) | LuceneError::MergeAborted(_) => !did_close,
      LuceneError::NoSuchFile(_) => !did_close,
      LuceneError::Io { source, .. } | LuceneError::IoWithPath { source, .. }
        if source.kind() == std::io::ErrorKind::NotFound
          || error.to_string().contains("aborted")
          || matches!(
            error.get_suppressed(),
            Ok(Some(LuceneError::MergeAborted(_)))
          ) =>
      {
        !did_close
      },
      LuceneError::Merge(_) if error.to_string().contains("aborted") => !did_close,
      _ => true,
    };
    if report {
      failures.lock().push(error.to_string());
    }
  }
}

// LUCENE-1335: test simultaneous addIndexes & close.
#[test]
fn test_add_indexes_with_close_no_wait() -> Result<()> {
  let mut random = random();
  const NUM_COPY: usize = 50;
  let mut c = CommitAndAddIndexes3::new(&mut random, NUM_COPY)?;
  c.launch_threads(&mut random, -1);

  thread::sleep(Duration::from_millis(random.random_range(10..=500)));

  // Close w/o first stopping/joining the threads.
  if cfg!(feature = "test_log_verbose") {
    println!("TEST: now close(false)");
  }
  c.base.close(false)?;

  c.base.join_threads()?;

  if cfg!(feature = "test_log_verbose") {
    println!("TEST: done join threads");
  }
  c.base.close_dir()?;
  let failures = c.base.failures.lock();
  assert!(failures.is_empty(), "unexpected failures: {failures:?}");
  Ok(())
}

#[test]
fn test_add_indexes_with_rollback() -> Result<()> {
  let mut random = random();
  let num_copy = if is_night_mode() { 50 } else { 5 };
  let mut c = CommitAndAddIndexes3::new(&mut random, num_copy)?;
  c.launch_threads(&mut random, -1);

  thread::sleep(Duration::from_millis(random.random_range(10..=500)));

  // Close w/o first stopping/joining the threads
  if cfg!(feature = "test_log_verbose") {
    println!("TEST: now force rollback");
  }
  c.base.did_close.store(true, Ordering::SeqCst);
  c.base.writer2.rollback()?;

  c.base.join_threads()?;

  if let MergeSchedulerEnum::Concurrent(ms) = c.base.writer2.get_config().get_merge_scheduler() {
    assert_eq!(0, ms.merge_thread_count());
  }

  c.base.close_dir()?;
  let failures = c.base.failures.lock();
  assert!(failures.is_empty(), "unexpected failures: {failures:?}");
  Ok(())
}

#[test]
fn test_existing_deletes() -> Result<()> {
  let mut random = random();
  let dirs = [
    new_directory_shared(&mut random)?,
    new_directory_shared(&mut random)?,
  ];
  for dir in &dirs {
    let analyzer = MockAnalyzer::new(&mut random);
    let conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
    let writer = IndexWriter::new(dir.clone(), conf)?;
    let mut doc = Document::new();
    doc.add(StringField::from_string("id", "myid", Store::No)?);
    writer.add_document(doc)?;
    writer.close()?;
  }

  let analyzer = MockAnalyzer::new(&mut random);
  let conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  let writer = IndexWriter::new(dirs[0].clone(), conf)?;

  // Now delete the document.
  writer.delete_documents_with_terms(vec![Term::from_text("id", "myid")])?;
  let reader = directory_reader::open(dirs[1].clone())?;
  TestUtil::add_indexes_slowly(&writer, std::slice::from_ref(&reader))?;
  reader.close()?;
  writer.commit()?;
  assert_eq!(
    1,
    writer.get_doc_stats()?.num_docs,
    "Documents from the incoming index should not have been deleted"
  );
  writer.close()?;

  for dir in &dirs {
    dir.close()?;
  }
  Ok(())
}

#[test]
fn test_simple_case_custom_codec() -> Result<()> {
  // TODO IMPORTANT setCodec未实现
  Ok(())
}
#[test]
fn test_non_cfs_leftovers() -> Result<()> {
  let mut random = random();
  let mut dirs = Vec::with_capacity(2);
  let mut field_types = HashMap::new();
  for _ in 0..2 {
    let dir: Arc<AddIndexesDirectory> = Arc::new(MockDirectoryWrapper::new(
      &mut random,
      Arc::new(ByteBuffersDirectory::new()),
    ));
    let analyzer = MockAnalyzer::new(&mut random);
    let conf = IndexWriterConfig::with_analyzer(analyzer)?;
    let writer = IndexWriter::new(dir.clone(), conf)?;
    let mut doc = Document::new();
    let mut custom_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_STORED)?;
    custom_type.set_store_term_vectors(true)?;
    doc.add(new_field(
      &mut random,
      "c",
      "v",
      &custom_type,
      &mut field_types,
    )?);
    writer.add_document(doc)?;
    writer.close()?;
    dirs.push(dir);
  }

  let readers = [
    directory_reader::open(dirs[0].clone())?,
    directory_reader::open(dirs[1].clone())?,
  ];

  let dir: Arc<AddIndexesDirectory> = Arc::new(MockDirectoryWrapper::new(
    &mut random,
    Arc::new(ByteBuffersDirectory::new()),
  ));
  let analyzer = MockAnalyzer::new(&mut random);
  let mut conf = IndexWriterConfig::with_analyzer(analyzer)?;
  let mut merge_policy = new_log_merge_policy_with_cfs(&mut random, true)?;
  MergePolicy::<AddIndexesDirectory>::get_base_mut(&mut merge_policy).set_no_cfs_ratio(1.0)?;
  MergePolicy::<AddIndexesDirectory>::get_base_mut(&mut merge_policy)
    .set_max_cfs_segment_size_mb(f64::INFINITY)?;
  conf.set_merge_policy(merge_policy);
  let writer = IndexWriter::new(dir.clone(), conf)?;
  TestUtil::add_indexes_slowly(&writer, &readers)?;
  writer.close()?;

  // We should now see segments_X, _Y.cfs, _Y.cfe, _Z.si.
  let segment_infos = SegmentInfos::read_latest_commit(dir.clone())?;
  assert_eq!(
    1,
    segment_infos.size(),
    "Only one compound segment should exist"
  );
  assert!(
    segment_infos
      .info(0)
      .expect("missing compound segment")
      .info
      .get_use_compound_file()
  );
  for reader in &readers {
    reader.close()?;
  }
  for source in &dirs {
    source.close()?;
  }
  dir.close()
}

#[test]
fn test_add_index_missing_codec() -> Result<()> {
  // TODO IMPORTANT setCodec未实现
  Ok(())
}

#[test]
fn test_field_names_changed() -> Result<()> {
  let mut random = random();
  let mut field_types = HashMap::new();

  let d1 = new_directory_shared(&mut random)?;
  let w = RandomIndexWriter::new(&mut random, d1.clone())?;
  let mut doc = Document::new();
  doc.add(new_string_field(
    &mut random,
    "f1",
    "doc1 field1",
    Store::Yes,
    &mut field_types,
  )?);
  doc.add(new_string_field(
    &mut random,
    "id",
    "1",
    Store::Yes,
    &mut field_types,
  )?);
  w.add_document(&mut random, doc)?;
  let r1 = w.get_reader(&mut random)?;
  w.close(&mut random)?;

  let d2 = new_directory_shared(&mut random)?;
  let w = RandomIndexWriter::new(&mut random, d2.clone())?;
  let mut doc = Document::new();
  doc.add(new_string_field(
    &mut random,
    "f2",
    "doc2 field2",
    Store::Yes,
    &mut field_types,
  )?);
  doc.add(new_string_field(
    &mut random,
    "id",
    "2",
    Store::Yes,
    &mut field_types,
  )?);
  w.add_document(&mut random, doc)?;
  let r2 = w.get_reader(&mut random)?;
  w.close(&mut random)?;

  let d3 = new_directory_shared(&mut random)?;
  let w = RandomIndexWriter::new(&mut random, d3.clone())?;
  w.add_indexes_from_codec_readers(
    &mut random,
    vec![
      SlowCodecReaderWrapper::wrap_leaf_reader(get_only_leaf_reader(&r1)?),
      SlowCodecReaderWrapper::wrap_leaf_reader(get_only_leaf_reader(&r2)?),
    ],
  )?;
  r1.close()?;
  d1.close()?;
  r2.close()?;
  d2.close()?;

  let r3 = w.get_reader(&mut random)?;
  w.close(&mut random)?;
  assert_eq!(2, r3.num_docs()?);
  let mut stored_fields = r3.stored_fields()?;
  for doc_id in 0..2 {
    let doc = stored_fields.document(doc_id)?;
    if doc.get("id")?.unwrap().as_ref() == "1" {
      assert_eq!("doc1 field1", doc.get("f1")?.unwrap().as_ref());
    } else {
      assert_eq!("doc2 field2", doc.get("f2")?.unwrap().as_ref());
    }
  }
  r3.close()?;
  d3.close()
}

#[test]
fn test_add_empty() -> Result<()> {
  let mut random = random();
  let d1 = new_directory_shared(&mut random)?;
  let w = RandomIndexWriter::new(&mut random, d1.clone())?;
  w.add_indexes_from_codec_readers(&mut random, Vec::<DefaultLeafReader<DirEnum>>::new())?;
  w.close(&mut random)?;

  let dr = directory_reader::open(d1)?;
  for leaf in (&dr).get_context()?.leaves()? {
    assert!(
      leaf.reader().max_doc()? > 0,
      "empty segments should be dropped by addIndexes"
    );
  }
  dr.close()?;
  Ok(())
}

#[test]
fn test_fake_all_deleted() -> Result<()> {
  let mut random = random();
  let src = new_directory_shared(&mut random)?;
  let dest = new_directory_shared(&mut random)?;
  let w = RandomIndexWriter::new(&mut random, src.clone())?;
  w.add_document(&mut random, Document::new())?;
  let source_reader = w.get_reader(&mut random)?;
  let source_reader_context = (&source_reader).get_context()?;
  let all_deleted_reader =
    AllDeletedFilterReader::new(source_reader_context.leaves()?[0].reader().clone())?;
  w.close(&mut random)?;

  let w = RandomIndexWriter::new(&mut random, dest.clone())?;
  w.add_indexes_from_codec_readers(
    &mut random,
    vec![SlowCodecReaderWrapper::wrap_leaf_reader(
      all_deleted_reader.clone(),
    )],
  )?;
  w.close(&mut random)?;
  let reader = directory_reader::open(src.clone())?;
  let reader_context = (&reader).get_context()?;
  for leaf in reader_context.leaves()? {
    assert!(
      leaf.reader().max_doc()? > 0,
      "empty segments should be dropped by addIndexes"
    );
  }
  reader.close()?;
  all_deleted_reader.close()?;
  src.close()?;
  dest.close()
}

#[test]
fn test_locks_block() -> Result<()> {
  let mut random = random();

  let src = new_directory_shared(&mut random)?;
  let w1 = RandomIndexWriter::new(&mut random, src.clone())?;
  w1.add_document(&mut random, Document::new())?;
  w1.commit(&mut random)?;

  let dest = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let iwc = new_index_writer_config_with_analyzer(&mut random, a)?;
  let w2 = RandomIndexWriter::with_config(&mut random, dest.clone(), iwc);

  let err = w2.add_indexes_from_dir(&mut random, std::slice::from_ref(&src));
  assert!(matches!(err, Err(LuceneError::LockObtainFailed(_))));

  w1.close(&mut random)?;
  w2.close(&mut random)?;
  Ok(())
}

#[test]
fn test_illegal_index_sort_change1() -> Result<()> {
  let mut random = random();

  let dir1 = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let mut iwc1 = new_index_writer_config_with_analyzer(&mut random, a)?;
  iwc1.set_index_sort(Sort::with_fields(vec![SortField::new(
    Some("foo"),
    SortFieldType::Int,
  )?])?)?;
  let w1 = RandomIndexWriter::with_config(&mut random, dir1.clone(), iwc1);
  w1.add_document(&mut random, Document::new())?;
  w1.commit(&mut random)?;
  w1.add_document(&mut random, Document::new())?;
  w1.commit(&mut random)?;
  w1.force_merge(&mut random, 1)?;
  w1.close(&mut random)?;
  drop(w1);

  let dir2 = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let mut iwc2 = new_index_writer_config_with_analyzer(&mut random, a)?;
  iwc2.set_index_sort(Sort::with_fields(vec![SortField::new(
    Some("foo"),
    SortFieldType::String,
  )?])?)?;
  let w2 = RandomIndexWriter::with_config(&mut random, dir2.clone(), iwc2);

  let err = w2.add_indexes_from_dir(&mut random, std::slice::from_ref(&dir1));
  assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
  assert_eq!(
    "cannot change index sort from <int: \"foo\"> to <string: \"foo\">",
    err.unwrap_err().to_string()
  );

  w2.close(&mut random)?;
  Ok(())
}

#[test]
fn test_illegal_index_sort_change2() -> Result<()> {
  let mut random = random();

  let dir1 = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let mut iwc1 = new_index_writer_config_with_analyzer(&mut random, a)?;
  iwc1.set_index_sort(Sort::with_fields(vec![SortField::new(
    Some("foo"),
    SortFieldType::Int,
  )?])?)?;
  let w1 = RandomIndexWriter::with_config(&mut random, dir1.clone(), iwc1);
  w1.add_document(&mut random, Document::new())?;
  w1.commit(&mut random)?;
  w1.add_document(&mut random, Document::new())?;
  w1.commit(&mut random)?;
  // so the index sort is in fact burned into the index:
  w1.force_merge(&mut random, 1)?;
  w1.close(&mut random)?;
  drop(w1);

  let dir2 = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let mut iwc2 = new_index_writer_config_with_analyzer(&mut random, a)?;
  iwc2.set_index_sort(Sort::with_fields(vec![SortField::new(
    Some("foo"),
    SortFieldType::String,
  )?])?)?;
  let w2 = RandomIndexWriter::with_config(&mut random, dir2, iwc2);
  let r1 = directory_reader::open(dir1)?;
  let reader = get_only_leaf_reader(&r1)?;
  let err = w2.add_indexes_from_codec_readers(&mut random, vec![reader]);
  assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
  assert_eq!(
    "cannot change index sort from <int: \"foo\"> to <string: \"foo\">",
    err.unwrap_err().to_string()
  );

  r1.close()?;
  w2.close(&mut random)?;
  Ok(())
}

#[test]
fn test_add_indexes_dv_update_same_segment_name() -> Result<()> {
  let mut random = random();

  let dir1 = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let iwc1 = new_index_writer_config_with_analyzer(&mut random, a)?;
  let w1 = IndexWriter::new(dir1.clone(), iwc1)?;

  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "1", Store::Yes)?);
  doc.add(StringField::from_string("version", "1", Store::Yes)?);
  doc.add(NumericDocValuesField::new("soft_delete", 1));
  w1.add_document(doc)?;
  w1.flush()?;

  w1.update_doc_values(
    Term::from_text("id", "1"),
    vec![NumericDocValuesField::new("soft_delete", 1).into()],
  )?;
  w1.commit()?;
  w1.close()?;
  drop(w1);
  let a = MockAnalyzer::new(&mut random);
  let iwc2 = new_index_writer_config_with_analyzer(&mut random, a)?;
  let dir2 = new_directory_shared(&mut random)?;
  let w2 = IndexWriter::new(dir2.clone(), iwc2)?;
  w2.add_indexes_from_directory(std::slice::from_ref(&dir1))?;
  w2.commit()?;
  w2.close()?;
  drop(w2);

  if cfg!(feature = "test_log_verbose") {
    println!("\nTEST: now open w3");
  }

  let a = MockAnalyzer::new(&mut random);
  let iwc3 = new_index_writer_config_with_analyzer(&mut random, a)?;
  let w3 = IndexWriter::new(dir2.clone(), iwc3)?;
  w3.close()?;
  drop(w3);
  let a = MockAnalyzer::new(&mut random);
  let iwc3 = new_index_writer_config_with_analyzer(&mut random, a)?;
  let w3 = IndexWriter::new(dir2.clone(), iwc3)?;
  w3.close()?;

  Ok(())
}

#[test]
fn test_add_indexes_dv_update_new_segment_name() -> Result<()> {
  let mut random = random();

  let dir1 = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let iwc1 = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  let w1 = IndexWriter::new(dir1.clone(), iwc1)?;
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "1", Store::Yes)?);
  doc.add(StringField::from_string("version", "1", Store::Yes)?);
  doc.add(NumericDocValuesField::new("soft_delete", 1));
  w1.add_document(doc)?;
  w1.flush()?;

  w1.update_doc_values(
    Term::from_text("id", "1"),
    vec![NumericDocValuesField::new("soft_delete", 1).into()],
  )?;
  w1.commit()?;
  w1.close()?;

  let analyzer = MockAnalyzer::new(&mut random);
  let iwc2 = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  let dir2 = new_directory_shared(&mut random)?;
  let w2 = IndexWriter::new(dir2.clone(), iwc2)?;
  w2.add_document(Document::new())?;
  w2.commit()?;
  w2.add_indexes_from_directory(std::slice::from_ref(&dir1))?;
  w2.commit()?;
  w2.close()?;

  if cfg!(feature = "test_log_verbose") {
    println!("\nTEST: now open w3");
  }
  let analyzer = MockAnalyzer::new(&mut random);
  let iwc3 = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  let w3 = IndexWriter::new(dir2.clone(), iwc3)?;
  w3.close()?;

  let analyzer = MockAnalyzer::new(&mut random);
  let iwc3 = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  let w3 = IndexWriter::new(dir2.clone(), iwc3)?;
  w3.close()?;
  dir1.close()?;
  dir2.close()
}

#[test]
fn test_add_indices_with_soft_deletes() -> Result<()> {
  let mut random = random();
  const SOFT_DELETES_FIELD: &str = "soft_delete";
  let dir1 = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc1 = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  iwc1.set_soft_deletes_field(SOFT_DELETES_FIELD);
  let writer = IndexWriter::new(dir1.clone(), iwc1)?;
  for _ in 0..30 {
    let mut doc = Document::new();
    let doc_id = random.random_range(0..5);
    doc.add(StringField::from_string(
      "id",
      doc_id.to_string(),
      Store::Yes,
    )?);
    writer.soft_update_document(
      Term::from_text("id", doc_id.to_string()),
      doc,
      vec![NumericDocValuesField::new(SOFT_DELETES_FIELD, 1).into()],
    )?;
    if random.random() {
      writer.flush()?;
    }
  }
  writer.commit()?;
  writer.close()?;

  let reader = Arc::new(directory_reader::open(dir1.clone())?);
  // wrapped_reader filters out soft deleted docs.
  let wrapped_reader = SoftDeletesDirectoryReaderWrapper::new(reader.clone(), SOFT_DELETES_FIELD)?;
  let dir2 = new_directory_shared(&mut random)?;
  let num_docs = reader.num_docs()?;
  let max_doc = reader.max_doc()?;
  assert_eq!(num_docs, max_doc);
  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  iwc.set_soft_deletes_field(SOFT_DELETES_FIELD);
  let writer = IndexWriter::new(dir2.clone(), iwc)?;
  let reader_context = (&reader).get_context()?;
  let leaves = reader_context.leaves()?;
  let mut readers = Vec::with_capacity(leaves.len());
  for leaf in leaves {
    readers.push(leaf.reader().clone());
  }
  writer.add_indexes_from_codec_readers(readers)?;
  assert_eq!(wrapped_reader.num_docs()?, writer.get_doc_stats()?.num_docs);
  assert_eq!(max_doc, writer.get_doc_stats()?.max_doc);
  writer.commit()?;
  let soft_delete_count: i32 = writer
    .clone_segment_infos()?
    .iter()
    .iter()
    .map(|info| info.get_soft_del_count())
    .sum();
  assert_eq!(max_doc - wrapped_reader.num_docs()?, soft_delete_count);
  writer.close()?;

  let dir3 = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  iwc.set_soft_deletes_field(SOFT_DELETES_FIELD);
  let writer = IndexWriter::new(dir3.clone(), iwc)?;
  // Resize as some fully deleted sub-readers might be dropped in wrapped_reader.
  let wrapped_reader_context = (&wrapped_reader).get_context()?;
  let leaves = wrapped_reader_context.leaves()?;
  let mut readers = Vec::with_capacity(leaves.len());
  for leaf in leaves {
    readers.push(leaf.reader().clone());
  }
  writer.add_indexes_from_codec_readers(readers)?;
  assert_eq!(wrapped_reader.num_docs()?, writer.get_doc_stats()?.num_docs);
  // Soft deletes got filtered out when wrapped readers were added.
  assert_eq!(wrapped_reader.num_docs()?, writer.get_doc_stats()?.max_doc);

  reader.close()?;
  wrapped_reader.close()?;
  writer.close()?;
  dir3.close()?;
  dir2.close()?;
  dir1.close()
}

#[test]
fn test_add_indices_with_blocks() -> Result<()> {
  let mut random = random();
  let add_has_blocks_perm = [true, true, false, false];
  let base_has_blocks_perm = [true, false, true, false];
  for perm in 0..add_has_blocks_perm.len() {
    let add_has_blocks = add_has_blocks_perm[perm];
    let base_has_blocks = base_has_blocks_perm[perm];
    let dir = new_directory_shared(&mut random)?;
    let writer = RandomIndexWriter::new(&mut random, dir.clone())?;
    let num_blocks = random.random_range(1..10);
    for _ in 0..num_blocks {
      let num_docs = if base_has_blocks {
        random.random_range(2..10)
      } else {
        1
      };
      let mut docs = Vec::with_capacity(num_docs);
      for _ in 0..num_docs {
        let mut doc = Document::new();
        let value = random.random_range(0..5);
        doc.add(StringField::from_string(
          "value",
          value.to_string(),
          Store::Yes,
        )?);
        docs.push(doc);
      }
      writer.add_documents(&mut random, docs)?;
    }
    writer.commit(&mut random)?;
    writer.close(&mut random)?;

    let add_dir = new_directory_shared(&mut random)?;
    let num_blocks = random.random_range(1..10);
    let writer = RandomIndexWriter::new(&mut random, add_dir.clone())?;
    for _ in 0..num_blocks {
      let num_docs = if add_has_blocks {
        random.random_range(2..10)
      } else {
        1
      };
      let mut docs = Vec::with_capacity(num_docs);
      for _ in 0..num_docs {
        let mut doc = Document::new();
        let value = random.random_range(0..5);
        doc.add(StringField::from_string(
          "value",
          value.to_string(),
          Store::Yes,
        )?);
        docs.push(doc);
      }
      writer.add_documents(&mut random, docs)?;
    }
    writer.commit(&mut random)?;
    writer.close(&mut random)?;

    let iwc = new_index_writer_config(&mut random)?;
    let writer = IndexWriter::new(dir.clone(), iwc)?;
    if random.random() {
      writer.add_indexes_from_directory(std::slice::from_ref(&add_dir))?;
    } else {
      let reader = directory_reader::open(add_dir.clone())?;
      let reader_context = (&reader).get_context()?;
      let leaves = reader_context.leaves()?;
      let mut readers = Vec::with_capacity(leaves.len());
      for leaf in leaves {
        readers.push(leaf.reader().clone());
      }
      writer.add_indexes_from_codec_readers(readers)?;
      reader.close()?;
    }
    writer.force_merge_with_wait(1, true)?;
    writer.close()?;

    let reader = directory_reader::open(dir.clone())?;
    let reader_context = (&reader).get_context()?;
    let leaves = reader_context.leaves()?;
    let codec_reader = leaves[0].reader();
    assert_eq!(1, leaves.len());
    let has_blocks = codec_reader.get_segment_info().info.get_has_blocks();
    if add_has_blocks || base_has_blocks {
      assert!(
        has_blocks,
        "add_has_blocks: {add_has_blocks} base_has_blocks: {base_has_blocks}"
      );
    } else {
      assert!(!has_blocks);
    }
    reader.close()?;
    add_dir.close()?;
    dir.close()?;
  }
  Ok(())
}

#[test]
fn test_set_diagnostics() -> Result<()> {
  let mut random = random();
  let source_dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  let writer = IndexWriter::new(source_dir.clone(), iwc)?;
  writer.add_document(Document::new())?;
  writer.close()?;

  let reader = directory_reader::open(source_dir.clone())?;
  let codec_reader = SlowCodecReaderWrapper::wrap_leaf_reader(get_only_leaf_reader(&reader)?);

  let target_dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  iwc.set_merge_policy(ConcurrentAddIndexesMergePolicy::diagnostics_merge_specification());
  let writer = IndexWriter::new(target_dir.clone(), iwc)?;
  writer.add_indexes_from_codec_readers(vec![codec_reader])?;
  writer.close()?;

  let segment_infos = SegmentInfos::read_latest_commit(target_dir.clone())?;
  assert_ne!(0, segment_infos.size());
  for info in segment_infos.iter() {
    assert_eq!(
      Some(&SOURCE_ADDINDEXES_READERS.to_string()),
      info.info.get_diagnostics().get(SOURCE)
    );
    assert_eq!(
      Some(&"my_merge_policy".to_string()),
      info.info.get_diagnostics().get("merge_policy")
    );
  }
  reader.close()?;
  target_dir.close()?;
  source_dir.close()
}

#[test]
fn test_illegal_parent_doc_change() -> Result<()> {
  let mut random = random();

  let dir1 = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc1 = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  iwc1.set_parent_field("foobar");
  let w1 = RandomIndexWriter::with_config(&mut random, dir1.clone(), iwc1);
  w1.add_documents(
    &mut random,
    vec![Document::new(), Document::new(), Document::new()],
  )?;
  w1.commit(&mut random)?;
  w1.add_documents(
    &mut random,
    vec![Document::new(), Document::new(), Document::new()],
  )?;
  w1.commit(&mut random)?;
  // So the parent field is in fact burned into the index.
  w1.force_merge(&mut random, 1)?;
  w1.close(&mut random)?;

  let dir2 = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc2 = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  iwc2.set_parent_field("foo");
  let w2 = RandomIndexWriter::with_config(&mut random, dir2.clone(), iwc2);

  let r1 = directory_reader::open(dir1.clone())?;
  let error = w2
    .add_indexes_from_codec_readers(&mut random, vec![get_only_leaf_reader(&r1)?])
    .expect_err("different parent fields must be rejected");
  assert_eq!(
    "can't add field [foobar] as parent document field; this IndexWriter is configured with [foo] as parent document field",
    error.to_string()
  );

  let error = w2
    .add_indexes_from_dir(&mut random, std::slice::from_ref(&dir1))
    .expect_err("different parent fields must be rejected");
  assert_eq!(
    "can't add field [foobar] as parent document field; this IndexWriter is configured with [foo] as parent document field",
    error.to_string()
  );

  let dir3 = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc3 = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  iwc3.set_parent_field("foobar");
  let w3 = RandomIndexWriter::with_config(&mut random, dir3.clone(), iwc3);
  w3.add_indexes_from_codec_readers(&mut random, vec![get_only_leaf_reader(&r1)?])?;
  w3.add_indexes_from_dir(&mut random, std::slice::from_ref(&dir1))?;

  r1.close()?;
  dir1.close()?;
  w2.close(&mut random)?;
  dir2.close()?;
  w3.close(&mut random)?;
  dir3.close()
}

#[test]
fn test_illegal_non_parent_field() -> Result<()> {
  let mut random = random();

  let dir1 = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let iwc1 = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  let w1 = RandomIndexWriter::with_config(&mut random, dir1.clone(), iwc1);
  let mut parent = Document::new();
  parent.add(StringField::from_string("foo", "XXX", Store::No)?);
  w1.add_document(&mut random, parent)?;
  w1.close(&mut random)?;

  let dir2 = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc2 = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  iwc2.set_parent_field("foo");
  let w2 = RandomIndexWriter::with_config(&mut random, dir2.clone(), iwc2);

  let r1 = directory_reader::open(dir1.clone())?;
  let error = w2
    .add_indexes_from_codec_readers(&mut random, vec![get_only_leaf_reader(&r1)?])
    .expect_err("a non-parent field cannot be added as the configured parent field");
  assert_eq!(
    "can't add [foo] as non parent document field; this IndexWriter is configured with [foo] as parent document field",
    error.to_string()
  );

  let error = w2
    .add_indexes_from_dir(&mut random, std::slice::from_ref(&dir1))
    .expect_err("a non-parent field cannot be added as the configured parent field");
  assert_eq!(
    "can't add [foo] as non parent document field; this IndexWriter is configured with [foo] as parent document field",
    error.to_string()
  );

  r1.close()?;
  dir1.close()?;
  w2.close(&mut random)?;
  dir2.close()
}
