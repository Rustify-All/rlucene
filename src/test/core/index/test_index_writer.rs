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
use crate::core::analysis::analyzer::{Analyzer, AnalyzerStoredValue, TokenStreamComponents};
use crate::core::analysis::reader::{Reader, StringReader};
use crate::core::analysis::token_attributes::payload_attribute::PayloadAttribute;
use crate::core::analysis::token_stream::TokenStream;
use crate::core::analysis::tokenizer::{Tokenizer, TokenizerBase};
use crate::core::document::document::Document;
use crate::core::document::field::{Field, Store};
use crate::core::document::field_type::FieldType;
use crate::core::document::fields::FieldTokenStreamEnum;
use crate::core::document::long_point::LongPoint;
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::document::sorted_doc_values_field::SortedDocValuesField;
use crate::core::document::sorted_numeric_doc_values_field::SortedNumericDocValuesField;
use crate::core::document::sorted_set_doc_values_field::SortedSetDocValuesField;
use crate::core::document::stored_field::StoredField;
use crate::core::document::string_field::StringField;
use crate::core::document::text_field::TextField;
use crate::core::index::check_index::{CheckIndex, Level};
use crate::core::index::concurrent_merge_scheduler::{
  ConcurrentMergeScheduler, ConcurrentMergeSchedulerHook,
};
use crate::core::index::directory_reader;
use crate::core::index::directory_reader::DirectoryReader;
use crate::core::index::doc_values_skip_index_type::DocValuesSkipIndexType;
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::documents_writer_per_thread::DocumentsWriterPerThread;
use crate::core::index::fields::Fields;
use crate::core::index::flush_policy::ApplyDeletesFlushPolicy;
use crate::core::index::index_commit::IndexCommit;
use crate::core::index::index_deletion_policy::IndexDeletionPolicyEnum;
use crate::core::index::index_file_deleter::CommitPoint;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::index_reader::{Identity, IndexReader};
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::MAX_STORED_STRING_LENGTH;
use crate::core::index::index_writer::MAX_TERM_LENGTH;
use crate::core::index::index_writer::{
  EventEnum, EventImplTest, EventQueue, IndexCommitWrapper, IndexWriter, IndexWriterHooks,
  IndexWriterHooksEnum, IntoFallibleIterator, WRITE_LOCK_NAME, read_field_infos,
};
use crate::core::index::index_writer_config::OpenMode;
use crate::core::index::index_writer_config::{DISABLE_AUTO_FLUSH, IndexWriterConfig};
use crate::core::index::indexable_field::IndexableField;
use crate::core::index::indexing_chain::IndexingChain;
use crate::core::index::keep_only_last_commit_deletion_policy::KeepOnlyLastCommitDeletionPolicy;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::lockable_concurrent_approximate_priority_queue::Lock;
use crate::core::index::log_doc_merge_policy::LogDocMergePolicy;
use crate::core::index::log_merge_policy::LogMergePolicy;
use crate::core::index::merge_policy::MergePolicy;
use crate::core::index::no_merge_policy::NoMergePolicy;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::one_merge_wrapping_merge_policy::OneMergeWrappingMergePolicy;
use crate::core::index::postings_enum::{ALL, FREQS, NONE, PostingsEnum};
use crate::core::index::segment_infos::SegmentInfos;
use crate::core::index::snapshot_deletion_policy::SnapshotDeletionPolicy;
use crate::core::index::soft_deletes_retention_merge_policy::SoftDeletesRetentionMergePolicy;
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::term::Term;
use crate::core::index::term_vectors::TermVectors;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::TermsEnum;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::index::{BytesRef, CODEC_FILE_PATTERN, IndexFileNames};
use crate::core::search::doc_id_set_iterator::{DocIdSetIterator, NO_MORE_DOCS};
use crate::core::search::match_all_docs_query::MatchAllDocsQuery;
use crate::core::search::phrase_query::Builder as PhraseQueryBuilder;
use crate::core::search::searcher_factory::SearcherFactory;
use crate::core::search::searcher_manager::SearcherManager;
use crate::core::search::term_query::TermQuery;
use crate::core::store::directory::{DirEnum, Directory};
use crate::core::store::{ByteBuffersDirectory, IndexOutput, NoLockFactory};
use crate::core::store::{DataOutput, IOContext, SimpleFSLockFactory};
use crate::core::util::attribute_source::{AttributeSource, Attributes};
use crate::core::util::automation::automata::Automata;
use crate::core::util::automation::automaton::Automaton;
use crate::core::util::automation::character_run_automaton::CharacterRunAutomaton;
use crate::core::util::bits::Bits;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::dummy::dummy_comparator::DummyComparator;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::info_stream::{InfoStream, InfoStreamEnum};
use crate::core::util::io_utils::IOUtils;
use crate::core::util::{HasIdentity, LATEST, StringHelper};
use crate::test_framework::core::analysis::canned_token_stream::CannedTokenStream;
use crate::test_framework::core::analysis::mock_analyzer::{MockAnalyzer, MockTokenFilter};
use crate::test_framework::core::analysis::mock_token_filter::ENGLISH_STOPSET;
use crate::test_framework::core::analysis::mock_tokenizer::{MockTokenizer, WHITESPACE};
use crate::test_framework::core::analysis::token;
pub use crate::test_framework::core::index::merge_policy::KeepFullyDeletedSegmentsMergePolicy;
use crate::test_framework::core::index::random_index_writer::{RandomIndexWriter, TestPoint};
use crate::test_framework::core::index::test_concurrent_merge_scheduler::CountDownLatch as ConcurrentMergeSchedulerCountDownLatch;
use crate::test_framework::core::index::test_index_writer::{
  AbortOnMergeCompleteOneMergeUnaryOperator, CloseWhileMergeIsRunningConcurrentMergeScheduler,
  MergeFinishedOnceOneMergeUnaryOperator, STORED_TEXT_TYPE, SoftUpdatesConcurrentlyMergePolicy,
  SoftUpdatesConcurrentlyOneMergeUnaryOperator, add_doc, add_doc_with_index,
  assert_no_unreferenced_files,
};
use crate::test_framework::core::store::base_directory_test_case::EXTRA_FILE_NAME;
use crate::test_framework::core::store::mock_directory_wrapper::{Failure, MockDirectoryWrapper};
use crate::test_framework::core::util::lucene_test_case::{
  at_least, call_stack_contains, create_temp_dir, get_only_leaf_reader, new_directory,
  new_directory_shared, new_field, new_fs_directory, new_fs_directory_with_lock_factory,
  new_index_writer_config, new_index_writer_config_with_analyzer, new_io_context,
  new_log_merge_policy, new_log_merge_policy_with_merge_factor, new_merge_policy,
  new_mock_directory, new_searcher_with_reader, new_snapshot_index_writer_config, new_string_field,
  new_text_field, random, random_from_seed, rarely, slow_file_exists,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::RngExt;
use rand_xoshiro::rand_core::Rng;
use std::clone::Clone;
use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter};
use std::sync::atomic::Ordering::SeqCst;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64};
use std::sync::{Arc, Barrier, Condvar, LazyLock, Mutex, mpsc};
use std::thread;
use std::vec;

#[allow(dead_code)]
pub(crate) struct TestIndexWriter;

#[test]
fn test_doc_count() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut field_types = HashMap::new();

  {
    let a = MockAnalyzer::new(&mut random);
    let config = new_index_writer_config_with_analyzer(&mut random, a)?;
    let writer = IndexWriter::new(dir.clone(), config)?;
    for i in 0..100 {
      add_doc_with_index(&mut random, &writer, i, &mut field_types)?;
      if random.random_bool(0.5) {
        writer.commit()?;
      }
    }
    let doc_stats = writer.get_doc_stats()?;
    assert_eq!(100, doc_stats.max_doc);
    assert_eq!(100, doc_stats.num_docs);
    writer.close()?;
  }

  {
    let mut config = new_index_writer_config(&mut random)?;
    config.set_merge_policy(KeepFullyDeletedSegmentsMergePolicy::default());
    let writer = IndexWriter::new(dir.clone(), config)?;
    for i in 0..40 {
      writer.delete_documents_with_terms(vec![Term::from_text("id", i.to_string())])?;
      if random.random_bool(0.5) {
        writer.commit()?;
      }
    }
    writer.flush()?;
    let doc_stats = writer.get_doc_stats()?;
    assert_eq!(100, doc_stats.max_doc);
    assert_eq!(60, doc_stats.num_docs);
    writer.close()?;
  }

  {
    let reader = directory_reader::open(dir.clone())?;
    assert_eq!(60, reader.num_docs()?);
    reader.close()?;
  }

  {
    let writer = IndexWriter::new(dir.clone(), new_index_writer_config(&mut random)?)?;
    assert_eq!(60, writer.get_doc_stats()?.num_docs);
    writer.force_merge(1)?;
    let doc_stats = writer.get_doc_stats()?;
    assert_eq!(60, doc_stats.max_doc);
    assert_eq!(60, doc_stats.num_docs);
    writer.close()?;
  }

  {
    let reader = directory_reader::open(dir.clone())?;
    assert_eq!(60, reader.max_doc()?);
    assert_eq!(60, reader.num_docs()?);
    reader.close()?;
  }

  {
    let mut config = new_index_writer_config(&mut random)?;
    config.set_open_mode(OpenMode::Create);
    let writer = IndexWriter::new(dir, config)?;
    let doc_stats = writer.get_doc_stats()?;
    assert_eq!(0, doc_stats.max_doc);
    assert_eq!(0, doc_stats.num_docs);
    writer.close()?;
  }
  Ok(())
}

#[test]
fn test_create_with_reader() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let mock = MockAnalyzer::new(&mut random);
  let config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let writer = IndexWriter::new(dir.clone(), config)?;
  let mut field_types = HashMap::new();
  add_doc(&mut random, &writer, &mut field_types)?;
  writer.close()?;
  drop(writer);

  let reader = directory_reader::open(dir.clone())?;
  assert_eq!(1, reader.num_docs()?);

  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config.set_open_mode(OpenMode::Create);

  let writer = IndexWriter::new(dir.clone(), config)?;
  assert_eq!(0, writer.get_doc_stats()?.max_doc);

  add_doc(&mut random, &writer, &mut field_types)?;
  writer.close()?;

  assert_eq!(1, reader.num_docs()?);

  let reader2 = directory_reader::open(dir)?;
  assert_eq!(1, reader2.num_docs()?);

  reader.close()?;
  reader2.close()?;

  Ok(())
}

#[test]
fn test_changes_after_close() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let writer = IndexWriter::new(dir, config)?;

  let mut field_types = HashMap::new();
  add_doc(&mut random, &writer, &mut field_types)?;

  writer.close()?;
  let err = add_doc(&mut random, &writer, &mut field_types);
  assert!(matches!(err, Err(LuceneError::AlreadyClosed(_))));

  Ok(())
}

#[test]
fn test_index_no_documents() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let writer = IndexWriter::new(dir.clone(), config)?;
  writer.commit()?;
  writer.close()?;
  drop(writer);

  let reader = directory_reader::open(dir.clone())?;
  assert_eq!(0, reader.max_doc()?);
  assert_eq!(0, reader.num_docs()?);
  reader.close()?;

  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config.set_open_mode(OpenMode::Append);
  let writer = IndexWriter::new(dir.clone(), config)?;
  writer.commit()?;
  writer.close()?;

  let reader = directory_reader::open(dir)?;
  assert_eq!(0, reader.max_doc()?);
  assert_eq!(0, reader.num_docs()?);
  reader.close()?;

  Ok(())
}

#[test]
fn test_small_ram_buffer() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config
    .set_ram_buffer_size_mb(0.000001)
    .set_merge_policy(new_log_merge_policy_with_merge_factor(&mut random, 10)?);
  let writer = IndexWriter::new(dir.clone(), config)?;
  let mut field_types = HashMap::new();

  let mut last_num_segments = get_segment_count(dir.clone())?;
  for j in 0..9 {
    let mut doc = Document::new();
    doc.add(new_field(
      &mut random,
      "field",
      format!("aaa{j}"),
      &STORED_TEXT_TYPE,
      &mut field_types,
    )?);
    writer.add_document(doc)?;
    // Verify that with a tiny RAM buffer we see new segment after every doc
    let num_segments = get_segment_count(dir.clone())?;
    assert!(num_segments > last_num_segments);
    last_num_segments = num_segments;
  }
  writer.close()?;
  Ok(())
}

/** Returns how many unique segment names are in the directory. */
fn get_segment_count<D>(dir: Arc<D>) -> Result<usize>
where
  D: Directory,
{
  let mut segments = HashSet::new();
  for file in dir.list_all()? {
    segments.insert(IndexFileNames::parse_segment_name(&file).to_string());
  }
  Ok(segments.len())
}

#[test]
fn test_changing_ram_buffer() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let writer = IndexWriter::new(
    dir.clone(),
    new_index_writer_config_with_analyzer(&mut random, mock)?,
  )?;
  writer.get_config_mut().set_max_buffered_docs(10);
  writer
    .get_config_mut()
    .set_ram_buffer_size_mb(DISABLE_AUTO_FLUSH as f64);
  let mut field_types = HashMap::new();

  let mut last_flush_count = -1;
  for j in 1..52 {
    let mut doc = Document::new();
    doc.add(new_field(
      &mut random,
      "field",
      format!("aaa{j}"),
      &STORED_TEXT_TYPE,
      &mut field_types,
    )?);
    writer.add_document(doc)?;
    TestUtil::sync_concurrent_merges(&writer)?;
    let flush_count = writer.get_flush_count();
    if j == 1 {
      last_flush_count = flush_count;
    } else if j < 10 {
      // No new files should be created
      assert_eq!(flush_count, last_flush_count);
    } else if j == 10 {
      assert!(flush_count > last_flush_count);
      last_flush_count = flush_count;
      writer.get_config_mut().set_ram_buffer_size_mb(0.000001);
      writer
        .get_config_mut()
        .set_max_buffered_docs(DISABLE_AUTO_FLUSH);
    } else if j < 20 {
      assert!(flush_count > last_flush_count);
      last_flush_count = flush_count;
    } else if j == 20 {
      writer.get_config_mut().set_ram_buffer_size_mb(16.0);
      writer
        .get_config_mut()
        .set_max_buffered_docs(DISABLE_AUTO_FLUSH);
      last_flush_count = flush_count;
    } else if j < 30 {
      assert_eq!(flush_count, last_flush_count);
    } else if j == 30 {
      writer.get_config_mut().set_ram_buffer_size_mb(0.000001);
      writer
        .get_config_mut()
        .set_max_buffered_docs(DISABLE_AUTO_FLUSH);
    } else if j < 40 {
      assert!(flush_count > last_flush_count);
      last_flush_count = flush_count;
    } else if j == 40 {
      writer.get_config_mut().set_max_buffered_docs(10);
      writer
        .get_config_mut()
        .set_ram_buffer_size_mb(DISABLE_AUTO_FLUSH as f64);
      last_flush_count = flush_count;
    } else if j < 50 {
      assert_eq!(flush_count, last_flush_count);
      writer.get_config_mut().set_max_buffered_docs(10);
      writer
        .get_config_mut()
        .set_ram_buffer_size_mb(DISABLE_AUTO_FLUSH as f64);
    } else if j == 50 {
      assert!(flush_count > last_flush_count);
    }
  }
  writer.close()?;
  Ok(())
}

#[test]
fn test_enabling_norms() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config.set_max_buffered_docs(10);
  let writer = IndexWriter::new(dir.clone(), config)?;

  let mut custom_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_STORED)?;
  custom_type.set_omit_norms(true)?;
  let mut field_types = HashMap::new();
  for j in 0..10 {
    let mut doc = Document::new();
    let f = if j != 8 {
      new_field(&mut random, "field", "aaa", &custom_type, &mut field_types)?
    } else {
      new_field(
        &mut random,
        "field",
        "aaa",
        &STORED_TEXT_TYPE,
        &mut field_types,
      )?
    };
    doc.add(f);
    writer.add_document(doc)?;
  }
  writer.close()?;
  drop(writer);

  let search_term = Term::from_text("field", "aaa");

  let reader = directory_reader::open(dir.clone())?;
  let searcher = new_searcher_with_reader(reader)?;
  let hits = searcher.search(TermQuery::new(search_term.clone()), 1000)?;
  assert_eq!(10, hits.score_docs.len());

  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config
    .set_open_mode(OpenMode::Create)
    .set_max_buffered_docs(10);
  let writer = IndexWriter::new(dir.clone(), config)?;

  for j in 0..27 {
    let mut doc = Document::new();
    let f = if j != 26 {
      new_field(&mut random, "field", "aaa", &custom_type, &mut field_types)?
    } else {
      new_field(
        &mut random,
        "field",
        "aaa",
        &STORED_TEXT_TYPE,
        &mut field_types,
      )?
    };
    doc.add(f);
    writer.add_document(doc)?;
  }
  writer.close()?;

  let reader = directory_reader::open(dir.clone())?;
  let searcher = new_searcher_with_reader(reader)?;
  let hits = searcher.search(TermQuery::new(search_term), 1000)?;
  assert_eq!(27, hits.score_docs.len());

  let reader = directory_reader::open(dir)?;
  reader.close()?;

  Ok(())
}

#[test]
fn test_high_freq_term() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config.set_ram_buffer_size_mb(0.01);
  let writer = IndexWriter::new(dir.clone(), config)?;

  let mut b = String::with_capacity(1024 * 1024);
  for _ in 0..4096 {
    b.push_str(" a a a a a a a a");
    b.push_str(" a a a a a a a a");
    b.push_str(" a a a a a a a a");
    b.push_str(" a a a a a a a a");
  }
  let mut doc = Document::new();
  let mut custom_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_STORED)?;
  custom_type.set_store_term_vectors(true)?;
  custom_type.set_store_term_vector_positions(true)?;
  custom_type.set_store_term_vector_offsets(true)?;
  doc.add(Field::new("field", b, custom_type));
  writer.add_document(doc)?;
  writer.close()?;

  let reader = directory_reader::open(dir)?;
  assert_eq!(1, reader.max_doc()?);
  assert_eq!(1, reader.num_docs()?);
  let t = Term::from_text("field", "a");
  assert_eq!(1, reader.doc_freq(&t)?);
  let mut td = TestUtil::docs_with_reader(
    &mut random,
    &reader,
    "field",
    &BytesRef::from_string("a"),
    None,
    FREQS as i32,
  )?
  .expect("term should exist");
  td.next_doc()?;
  assert_eq!(128 * 1024, td.freq()?);
  reader.close()?;

  Ok(())
}

#[test]
fn test_flush_with_no_merging() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config
    .set_max_buffered_docs(2)
    .set_merge_policy(new_log_merge_policy_with_merge_factor(&mut random, 10)?);
  let writer = IndexWriter::new(dir, config)?;

  let mut doc = Document::new();
  let mut custom_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_STORED)?;
  custom_type.set_store_term_vectors(true)?;
  custom_type.set_store_term_vector_positions(true)?;
  custom_type.set_store_term_vector_offsets(true)?;
  doc.add(Field::new("field", "aaa", custom_type));
  for _ in 0..19 {
    writer.add_document(doc.clone())?;
  }
  writer.flush_with_apply_merge_deletes(false, true)?;
  assert_eq!(10, writer.get_segment_count());
  writer.close()?;

  Ok(())
}

#[test]
fn test_empty_doc_after_flushing_real_doc() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let writer = IndexWriter::new(dir.clone(), config)?;

  let mut field_types = HashMap::new();
  let mut doc = Document::new();

  let mut custom_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_STORED)?;
  custom_type.set_store_term_vectors(true)?;
  custom_type.set_store_term_vector_positions(true)?;
  custom_type.set_store_term_vector_offsets(true)?;

  doc.add(new_field(
    &mut random,
    "field",
    "aaa",
    &custom_type,
    &mut field_types,
  )?);
  writer.add_document(doc)?;
  writer.commit()?;
  if cfg!(feature = "test_log_verbose") {
    println!("TEST: now add empty doc");
  }
  let empty_doc = Document::new();
  writer.add_document(empty_doc)?;
  writer.close()?;
  let reader = directory_reader::open(dir.clone())?;
  assert_eq!(2, reader.num_docs()?);

  Ok(())
}

#[test]
fn test_bad_segment() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let writer = IndexWriter::new(dir.clone(), config)?;

  let mut field_types = HashMap::new();
  let mut doc = Document::new();
  let mut custom_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  custom_type.set_store_term_vectors(true)?;
  doc.add(new_field(
    &mut random,
    "tvtest",
    "",
    &custom_type,
    &mut field_types,
  )?);

  writer.add_document(doc)?;
  writer.close()?;
  Ok(())
}

#[test]
#[ignore = "Java-only: Rust's standard thread API has no thread-priority equivalent"]
fn test_max_thread_priority() -> Result<()> {
  test_not_required_in_rust_lucene!();
}

#[test]
fn test_variable_schema() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mut field_types = HashMap::new();

  for i in 0..20 {
    let mock = MockAnalyzer::new(&mut random);
    let mut iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
    iwc.set_max_buffered_docs(2);
    iwc.set_merge_policy(new_log_merge_policy(&mut random)?);

    let writer = IndexWriter::new(dir.clone(), iwc)?;

    let mut doc = Document::new();
    let contents = "aa bb cc dd ee ff gg hh ii jj kk";

    let custom_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_STORED)?;

    if i == 7 {
      doc.add(new_text_field(
        &mut random,
        "content3",
        "",
        Store::No,
        &mut field_types,
      )?);
    } else {
      let field_type = if i % 2 == 0 {
        doc.add(new_field(
          &mut random,
          "content4",
          contents,
          &custom_type,
          &mut field_types,
        )?);
        custom_type.clone()
      } else {
        FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?
      };

      doc.add(new_text_field(
        &mut random,
        "content1",
        contents,
        Store::No,
        &mut field_types,
      )?);

      doc.add(new_field(
        &mut random,
        "content3",
        "",
        &custom_type,
        &mut field_types,
      )?);

      doc.add(new_field(
        &mut random,
        "content5",
        "",
        &field_type,
        &mut field_types,
      )?);
    }

    for _ in 0..4 {
      writer.add_document(doc.clone())?;
    }

    writer.close()?;
    drop(writer);

    if i % 4 == 0 {
      let mock = MockAnalyzer::new(&mut random);
      let iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
      let writer = IndexWriter::new(dir.clone(), iwc)?;
      writer.force_merge(1)?;
      writer.close()?;
    }
  }

  Ok(())
}

#[test]
fn test_unlimited_max_field_length() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let writer = IndexWriter::new(dir.clone(), config)?;

  let mut field_types = HashMap::new();
  let mut doc = Document::new();
  let text = " a".repeat(10_000) + " x";
  doc.add(new_text_field(
    &mut random,
    "field",
    &text,
    Store::No,
    &mut field_types,
  )?);
  writer.add_document(doc)?;
  writer.close()?;

  let reader = directory_reader::open(dir.clone())?;
  let t = Term::from_text("field", "x");
  assert_eq!(1, reader.doc_freq(&t)?);
  Ok(())
}

#[test]
fn test_empty_field_name() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let writer = IndexWriter::new(dir.clone(), config)?;

  let mut field_types = HashMap::new();
  let mut doc = Document::new();
  doc.add(new_text_field(
    &mut random,
    "",
    "a b c",
    Store::No,
    &mut field_types,
  )?);
  writer.add_document(doc)?;
  writer.close()?;

  Ok(())
}

#[test]
fn test_empty_field_name_terms() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let mock = MockAnalyzer::new(&mut random);
  let config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let writer = IndexWriter::new(dir.clone(), config)?;

  let mut field_to_type = HashMap::new();
  let mut doc = Document::new();
  doc.add(new_text_field(
    &mut random,
    "",
    "a b c",
    Store::No,
    &mut field_to_type,
  )?);

  writer.add_document(doc)?;
  writer.close()?;

  let reader = directory_reader::open(dir)?;
  let subreader = get_only_leaf_reader(&reader)?;

  let terms = LeafReader::terms(&subreader, "")?.unwrap();
  let mut te = terms.iterator()?;

  assert_eq!(&BytesRef::from_string("a"), te.next()?.unwrap().as_ref());
  assert_eq!(&BytesRef::from_string("b"), te.next()?.unwrap().as_ref());
  assert_eq!(&BytesRef::from_string("c"), te.next()?.unwrap().as_ref());
  assert_eq!(None, te.next()?);

  Ok(())
}

#[test]
fn test_empty_field_name_with_empty_term() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let mock = MockAnalyzer::new(&mut random);
  let config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let writer = IndexWriter::new(dir.clone(), config)?;

  let mut field_to_type = HashMap::new();
  let mut doc = Document::new();

  doc.add(new_string_field(
    &mut random,
    "",
    "",
    Store::No,
    &mut field_to_type,
  )?);
  doc.add(new_string_field(
    &mut random,
    "",
    "a",
    Store::No,
    &mut field_to_type,
  )?);
  doc.add(new_string_field(
    &mut random,
    "",
    "b",
    Store::No,
    &mut field_to_type,
  )?);
  doc.add(new_string_field(
    &mut random,
    "",
    "c",
    Store::No,
    &mut field_to_type,
  )?);

  writer.add_document(doc)?;
  writer.close()?;

  let reader = directory_reader::open(dir)?;
  let subreader = get_only_leaf_reader(&reader)?;

  let terms = LeafReader::terms(&subreader, "")?.unwrap();
  let mut te = terms.iterator()?;

  assert_eq!(&BytesRef::from_string(""), te.next()?.unwrap().as_ref());
  assert_eq!(&BytesRef::from_string("a"), te.next()?.unwrap().as_ref());
  assert_eq!(&BytesRef::from_string("b"), te.next()?.unwrap().as_ref());
  assert_eq!(&BytesRef::from_string("c"), te.next()?.unwrap().as_ref());
  assert_eq!(None, te.next()?);

  Ok(())
}

#[test]
fn test_do_before_after_flush() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock_index_writer = MockIndexWriter::new();
  let before_was_called = mock_index_writer.before_was_called.clone();
  let after_was_called = mock_index_writer.after_was_called.clone();
  let writer = IndexWriter::with_hooks(
    dir.clone(),
    new_index_writer_config(&mut random)?,
    Some(IndexWriterHooksEnum::custom(mock_index_writer)),
  )?;

  let mut field_types = HashMap::new();
  let mut doc = Document::new();
  let custom_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_STORED)?;
  doc.add(new_field(
    &mut random,
    "field",
    "a field",
    &custom_type,
    &mut field_types,
  )?);
  writer.add_document(doc)?;
  writer.commit()?;

  assert!(before_was_called.load(SeqCst));
  assert!(after_was_called.load(SeqCst));
  before_was_called.store(false, SeqCst);
  after_was_called.store(false, SeqCst);

  writer.delete_documents_with_terms(vec![Term::from_text("field", "field"); 1])?;
  writer.commit()?;

  assert!(before_was_called.load(SeqCst));
  assert!(after_was_called.load(SeqCst));

  writer.close()?;

  let reader = directory_reader::open(dir.clone())?;
  assert_eq!(0, reader.num_docs()?);

  Ok(())
}

#[test]
fn test_negative_positions() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  let writer = IndexWriter::new(dir, iwc)?;

  let mut doc = Document::new();
  doc.add(TextField::from_token_stream(
    "field",
    FieldTokenStreamEnum::custom(NegativePositionsTokenStream::new()),
  )?);

  let result = writer.add_document(doc);
  assert!(matches!(result, Err(LuceneError::IllegalArgument(_))));

  writer.close()?;
  Ok(())
}

#[test]
fn test_position_increment_gap_empty_field() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;

  let mut analyzer = MockAnalyzer::new(&mut random);
  analyzer.set_position_increment_gap(100);

  let iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let mut field_types = HashMap::new();

  let mut custom_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  custom_type.set_store_term_vectors(true)?;
  custom_type.set_store_term_vector_positions(true)?;

  let mut doc = Document::new();
  doc.add(new_field(
    &mut random,
    "field",
    "",
    &custom_type,
    &mut field_types,
  )?);
  doc.add(new_field(
    &mut random,
    "field",
    "crunch man",
    &custom_type,
    &mut field_types,
  )?);

  w.add_document(doc)?;
  w.close()?;

  let r = directory_reader::open(dir)?;
  let mut term_vectors = r.term_vectors()?;
  let fields = term_vectors.get(0)?.unwrap();
  let tpv = fields.terms("field")?.unwrap();

  let mut terms_enum = tpv.iterator()?;

  assert!(terms_enum.next()?.is_some());
  let mut dp_enum = terms_enum.postings_with_flags(None, ALL as i32)?;
  assert_ne!(NO_MORE_DOCS, dp_enum.next_doc()?);
  assert_eq!(1, dp_enum.freq()?);
  assert_eq!(100, dp_enum.next_position()?);

  assert!(terms_enum.next()?.is_some());
  let mut dp_enum = terms_enum.postings_with_flags(Some(dp_enum), ALL as i32)?;
  assert_ne!(NO_MORE_DOCS, dp_enum.next_doc()?);
  assert_eq!(1, dp_enum.freq()?);
  assert_eq!(101, dp_enum.next_position()?);

  assert!(terms_enum.next()?.is_none());

  Ok(())
}

#[test]
fn test_deadlock() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;

  let mock = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  iwc.set_max_buffered_docs(2);

  let writer = IndexWriter::new(dir.clone(), iwc)?;

  let mut field_types = HashMap::new();

  let mut custom_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_STORED)?;
  custom_type.set_store_term_vectors(true)?;
  custom_type.set_store_term_vector_positions(true)?;
  custom_type.set_store_term_vector_offsets(true)?;

  let mut doc = Document::new();
  doc.add(new_field(
    &mut random,
    "content",
    "aaa bbb ccc ddd eee fff ggg hhh iii",
    &custom_type,
    &mut field_types,
  )?);

  writer.add_document(doc.clone())?;
  writer.add_document(doc.clone())?;
  writer.add_document(doc.clone())?;
  writer.commit()?;

  let dir2 = new_directory_shared(&mut random)?;

  let mock = MockAnalyzer::new(&mut random);
  let iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let writer2 = IndexWriter::new(dir2.clone(), iwc)?;
  writer2.add_document(doc)?;
  writer2.close()?;

  let r1 = directory_reader::open(dir2.clone())?;
  TestUtil::add_indexes_slowly(&writer, &[&r1, &r1])?;
  writer.close()?;

  let r3 = directory_reader::open(dir.clone())?;
  assert_eq!(5, r3.num_docs()?);
  r3.close()?;
  r1.close()?;

  Ok(())
}

#[test]
#[ignore = "Java-only: Rust threads have no interrupt flag or InterruptedException semantics"]
fn test_thread_interrupt_deadlock() -> Result<()> {
  test_not_required_in_rust_lucene!();
}

#[test]
fn test_index_store_combos() -> Result<()> {
  let mut rng = random();
  let dir = new_directory_shared(&mut rng)?;
  let mock = MockAnalyzer::new(&mut rng);
  let iwc = new_index_writer_config_with_analyzer(&mut rng, mock)?;
  let writer = IndexWriter::new(dir.clone(), iwc)?;
  let b: Vec<u8> = (0..50).map(|i| i + 77).collect();

  let mut custom_type = FieldType::new();
  custom_type.set_tokenized(true)?;
  custom_type.set_index_options(IndexOptions::Docs)?;

  let custom_type2 = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;

  let r = random_from_seed(rng.random());
  let mut field1 = MockTokenizer::with_default_max_token_length(r, WHITESPACE.clone(), false);
  field1.set_reader(StringReader::new("doc1field1").into())?;
  let r = random_from_seed(rng.random());
  let mut field2 = MockTokenizer::with_default_max_token_length(r, WHITESPACE.clone(), false);
  field2.set_reader(StringReader::new("doc1field2").into())?;
  let mut doc = Document::new();
  doc.add(StoredField::from_binary_with_range(
    "binary",
    b.clone(),
    10,
    17,
  )?);
  doc.add(Field::from_token_stream(
    "binary",
    FieldTokenStreamEnum::custom(field1),
    custom_type.clone(),
  )?);
  doc.add(Field::from_string(
    "string",
    "value",
    FieldType::from_ref(&*crate::core::document::text_field::TYPE_STORED)?,
  )?);
  doc.add(Field::from_token_stream(
    "string",
    FieldTokenStreamEnum::custom(field2),
    custom_type2.clone(),
  )?);
  writer.add_document(doc)?;

  let r = random_from_seed(rng.random());
  let mut field1 = MockTokenizer::with_default_max_token_length(r, WHITESPACE.clone(), false);
  field1.set_reader(StringReader::new("doc2field1").into())?;
  let r = random_from_seed(rng.random());
  let mut field2 = MockTokenizer::with_default_max_token_length(r, WHITESPACE.clone(), false);
  field2.set_reader(StringReader::new("doc2field2").into())?;
  let mut doc = Document::new();
  doc.add(StoredField::from_binary_with_range(
    "binary",
    b.clone(),
    10,
    17,
  )?);
  doc.add(Field::from_token_stream(
    "binary",
    FieldTokenStreamEnum::custom(field1),
    custom_type.clone(),
  )?);
  doc.add(Field::from_string(
    "string",
    "value",
    FieldType::from_ref(&*crate::core::document::text_field::TYPE_STORED)?,
  )?);
  doc.add(Field::from_token_stream(
    "string",
    FieldTokenStreamEnum::custom(field2),
    custom_type2.clone(),
  )?);
  writer.add_document(doc)?;

  writer.commit()?;
  let r = random_from_seed(rng.random());
  let mut field1 = MockTokenizer::with_default_max_token_length(r, WHITESPACE.clone(), false);
  field1.set_reader(StringReader::new("doc3field1").into())?;
  let r = random_from_seed(rng.random());
  let mut field2 = MockTokenizer::with_default_max_token_length(r, WHITESPACE.clone(), false);
  field2.set_reader(StringReader::new("doc3field2").into())?;
  let mut doc = Document::new();
  doc.add(StoredField::from_binary_with_range(
    "binary",
    b.clone(),
    10,
    17,
  )?);
  doc.add(Field::from_token_stream(
    "binary",
    FieldTokenStreamEnum::custom(field1),
    custom_type,
  )?);
  doc.add(Field::from_string(
    "string",
    "value",
    FieldType::from_ref(&*crate::core::document::text_field::TYPE_STORED)?,
  )?);
  doc.add(Field::from_token_stream(
    "string",
    FieldTokenStreamEnum::custom(field2),
    custom_type2,
  )?);
  writer.add_document(doc)?;
  writer.commit()?;
  writer.force_merge(1)?;
  writer.close()?;

  let reader = directory_reader::open(dir)?;
  let mut stored_fields = reader.stored_fields()?;
  let doc2 = stored_fields.document(0)?;
  let f3 = doc2.get_field("binary").expect("binary field should exist");
  let b = f3.binary_value()?.expect("binary value should exist");
  assert_eq!(17, b.length);
  assert_eq!(87, b.bytes[b.offset]);

  for doc_id in 0..3 {
    assert!(
      stored_fields
        .document(doc_id)?
        .get_field("binary")
        .expect("binary field should exist")
        .binary_value()?
        .is_some()
    );
  }

  assert_eq!(
    "value",
    stored_fields.document(0)?.get("string")?.unwrap().as_ref()
  );
  assert_eq!(
    "value",
    stored_fields.document(1)?.get("string")?.unwrap().as_ref()
  );
  assert_eq!(
    "value",
    stored_fields.document(2)?.get("string")?.unwrap().as_ref()
  );

  assert_ne!(
    TestUtil::docs_with_reader(
      &mut rng,
      &reader,
      "binary",
      &BytesRef::from_string("doc1field1"),
      None,
      NONE as i32,
    )?
    .expect("term should exist")
    .next_doc()?,
    NO_MORE_DOCS
  );
  assert_ne!(
    TestUtil::docs_with_reader(
      &mut rng,
      &reader,
      "binary",
      &BytesRef::from_string("doc2field1"),
      None,
      NONE as i32,
    )?
    .expect("term should exist")
    .next_doc()?,
    NO_MORE_DOCS
  );
  assert_ne!(
    TestUtil::docs_with_reader(
      &mut rng,
      &reader,
      "binary",
      &BytesRef::from_string("doc3field1"),
      None,
      NONE as i32,
    )?
    .expect("term should exist")
    .next_doc()?,
    NO_MORE_DOCS
  );
  assert_ne!(
    TestUtil::docs_with_reader(
      &mut rng,
      &reader,
      "string",
      &BytesRef::from_string("doc1field2"),
      None,
      NONE as i32,
    )?
    .expect("term should exist")
    .next_doc()?,
    NO_MORE_DOCS
  );
  assert_ne!(
    TestUtil::docs_with_reader(
      &mut rng,
      &reader,
      "string",
      &BytesRef::from_string("doc2field2"),
      None,
      NONE as i32,
    )?
    .expect("term should exist")
    .next_doc()?,
    NO_MORE_DOCS
  );
  assert_ne!(
    TestUtil::docs_with_reader(
      &mut rng,
      &reader,
      "string",
      &BytesRef::from_string("doc3field2"),
      None,
      NONE as i32,
    )?
    .expect("term should exist")
    .next_doc()?,
    NO_MORE_DOCS
  );

  reader.close()?;
  Ok(())
}

#[test]
fn test_no_docs_index() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let writer = IndexWriter::new(dir.clone(), iwc)?;
  writer.add_document(Document::new())?;
  writer.close()?;

  Ok(())
}

#[test]
fn test_delete_unused_files() -> Result<()> {
  // TODO: WindowsFS is not implemented, so delete-on-last-close pending-file behavior cannot be
  // exercised.
  test_not_required_in_rust_lucene!();
}

#[test]
fn test_delete_unused_files2() -> Result<()> {
  // Validates that iw.deleteUnusedFiles() also deletes unused index commits
  // in case a deletion policy which holds onto commits is used.
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config.set_index_deletion_policy(SnapshotDeletionPolicy::new(
    KeepOnlyLastCommitDeletionPolicy,
  ));
  let writer = IndexWriter::new(dir.clone(), config)?;
  let sdp = match writer.get_config().get_index_deletion_policy() {
    IndexDeletionPolicyEnum::Snapshot(policy) => policy.as_ref(),
    policy => {
      return Err(LuceneError::illegal_state(format!(
        "expected SnapshotDeletionPolicy but got {policy}"
      )));
    },
  };

  // First commit
  let mut doc = Document::new();

  let mut custom_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_STORED)?;
  custom_type.set_store_term_vectors(true)?;
  custom_type.set_store_term_vector_positions(true)?;
  custom_type.set_store_term_vector_offsets(true)?;

  let mut field_types = HashMap::new();
  doc.add(new_field(
    &mut random,
    "c",
    "val",
    &custom_type,
    &mut field_types,
  )?);
  writer.add_document(doc)?;
  writer.commit()?;
  assert_eq!(1, directory_reader::list_commits(dir.clone())?.len());

  // Keep that commit
  let id = sdp.snapshot()?;

  // Second commit - now KeepOnlyLastCommit cannot delete the prev commit.
  let mut doc = Document::new();
  doc.add(new_field(
    &mut random,
    "c",
    "val",
    &custom_type,
    &mut field_types,
  )?);
  writer.add_document(doc)?;
  writer.commit()?;
  assert_eq!(2, directory_reader::list_commits(dir.clone())?.len());

  // Should delete the unreferenced commit
  sdp.release(&id)?;
  writer.delete_unused_files()?;
  assert_eq!(1, directory_reader::list_commits(dir.clone())?.len());

  writer.close()?;
  Ok(())
}

#[test]
fn test_empty_fs_dir_with_no_lock() -> Result<()> {
  // Tests that if FSDir is opened w/ a NoLockFactory (or SingleInstanceLF),
  // then IndexWriter ctor succeeds. Previously (LUCENE-2386) it failed
  // when listAll() was called in IndexFileDeleter.
  let mut random = random();
  let temp_dir = create_temp_dir()?;
  let dir = Arc::new(new_fs_directory_with_lock_factory(
    &mut random,
    temp_dir.keep(),
    NoLockFactory,
  )?);
  let a = MockAnalyzer::new(&mut random);
  IndexWriter::new(dir, new_index_writer_config_with_analyzer(&mut random, a)?)?.close()?;
  Ok(())
}

#[test]
fn test_empty_dir_rollback() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let orig_files = dir.list_all()?;
  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config.set_max_buffered_docs(2);
  config.set_merge_policy(new_log_merge_policy(&mut random)?);
  config.set_use_compound_file(false);
  let writer = IndexWriter::new(dir.clone(), config)?;
  let mut files = dir.list_all()?;

  let extra_file_count = files.len() - orig_files.len();
  if extra_file_count == 1 {
    assert!(files.contains(&WRITE_LOCK_NAME.to_string()));
  } else {
    let mut sorted_orig_files = orig_files.clone();
    sorted_orig_files.sort();
    files.sort();
    assert_eq!(sorted_orig_files, files);
  }

  let mut custom_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_STORED)?;
  custom_type.set_store_term_vectors(true)?;
  custom_type.set_store_term_vector_positions(true)?;
  custom_type.set_store_term_vector_offsets(true)?;

  let mut field_types = HashMap::new();
  let mut doc = Document::new();
  doc.add(new_field(
    &mut random,
    "c",
    "val",
    &custom_type,
    &mut field_types,
  )?);
  writer.add_document(doc)?;

  let mut computed_extra_file_count = 0;
  for file in dir.list_all()? {
    if file == WRITE_LOCK_NAME
      || file.starts_with(IndexFileNames::SEGMENTS)
      || CODEC_FILE_PATTERN.is_match(&file)
    {
      let should_count = match file.rsplit_once('.') {
        None => true,
        Some((_, ext)) => !matches!(ext, "fdm" | "fdt" | "tvm" | "tvd" | "tmp"),
      };
      if should_count {
        computed_extra_file_count += 1;
      }
    }
  }
  assert_eq!(
    extra_file_count, computed_extra_file_count,
    "only the stored and term vector files should exist in the directory"
  );

  let mut doc = Document::new();
  doc.add(new_field(
    &mut random,
    "c",
    "val",
    &custom_type,
    &mut field_types,
  )?);
  writer.add_document(doc)?;

  assert!(
    dir.list_all()?.len() > 5 + extra_file_count,
    "flush should have occurred and files should have been created"
  );

  writer.rollback()?;
  let all_files = dir.list_all()?;
  assert_eq!(
    orig_files.len() + extra_file_count,
    all_files.len(),
    "no files should exist in the directory after rollback"
  );

  writer.close()?;
  let all_files = dir.list_all()?;
  assert_eq!(
    orig_files.len() + extra_file_count,
    all_files.len(),
    "expected a no-op close after IW.rollback()"
  );

  Ok(())
}

#[test]
fn test_no_unwanted_tv_files() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config.set_ram_buffer_size_mb(0.01);
  let mut merge_policy = new_log_merge_policy(&mut random)?;
  merge_policy.get_base_mut().set_no_cfs_ratio(0.0)?;
  config.set_merge_policy(merge_policy);
  let writer = IndexWriter::new(dir.clone(), config)?;

  let mut big =
    "alskjhlaksjghlaksjfhalksvjepgjioefgjnsdfjgefgjhelkgjhqewlrkhgwlekgrhwelkgjhwelkgrhwlkejg"
      .to_string();
  big = big.repeat(4);

  let mut custom_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_STORED)?;
  custom_type.set_omit_norms(true)?;
  let mut custom_type2 = FieldType::from_ref(&*crate::core::document::text_field::TYPE_STORED)?;
  custom_type2.set_tokenized(false)?;
  let mut custom_type3 = FieldType::from_ref(&*crate::core::document::text_field::TYPE_STORED)?;
  custom_type3.set_tokenized(false)?;
  custom_type3.set_omit_norms(true)?;

  for i in 0..2 {
    let text = format!("{i}{big}");
    let mut doc = Document::new();
    doc.add(Field::from_string(
      "id",
      text.clone(),
      custom_type3.clone(),
    )?);
    doc.add(Field::from_string(
      "str",
      text.clone(),
      custom_type2.clone(),
    )?);
    doc.add(Field::from_string(
      "str2",
      text.clone(),
      STORED_TEXT_TYPE.clone(),
    )?);
    doc.add(Field::from_string("str3", text, custom_type.clone())?);
    writer.add_document(doc)?;
  }

  writer.close()?;
  drop(writer);

  TestUtil::check_index(&mut random, dir.clone())?;

  assert_no_unreferenced_files(dir.clone(), "no tv files")?;

  let reader = directory_reader::open(dir)?;
  let context = (&reader).get_context()?;
  for ctx in context.leaves()? {
    assert!(!ctx.reader().get_field_infos()?.has_term_vectors());
  }

  reader.close()?;
  Ok(())
}

struct StringSplitAnalyzer {
  stored_value: AnalyzerStoredValue,
}

impl Analyzer for StringSplitAnalyzer {
  fn create_components(&self, _field_name: &str) -> Result<TokenStreamComponents> {
    Ok(TokenStreamComponents::new(
      Box::new(StringSplitTokenizer::new()) as Box<dyn TokenStream + Send + Sync>,
      None,
    ))
  }

  fn stored_value(&self) -> &AnalyzerStoredValue {
    &self.stored_value
  }
}

crate::impl_analyzer_close!(StringSplitAnalyzer);

struct StringSplitTokenizer {
  tokenizer_base: TokenizerBase,
  tokens: Vec<String>,
  upto: usize,
}

impl StringSplitTokenizer {
  fn new() -> Self {
    Self {
      tokenizer_base: TokenizerBase::new(Attributes::default()),
      tokens: Vec::new(),
      upto: 0,
    }
  }
}

impl Closeable for StringSplitTokenizer {
  fn close(&mut self) -> Result<()> {
    self.tokenizer_base.close()
  }
}

impl TokenStream for StringSplitTokenizer {
  fn increment_token(&mut self) -> Result<bool> {
    if self.upto == self.tokens.len() {
      return Ok(false);
    }
    let term = &self.tokens[self.upto];
    self
      .tokenizer_base
      .token_stream_base
      .att
      .clear_attributes()?;
    self.tokenizer_base.token_stream_base.att.set_empty()?;
    self
      .tokenizer_base
      .token_stream_base
      .att
      .append_str(Some(term))?;
    self.upto += 1;
    Ok(true)
  }

  fn end(&mut self) -> Result<()> {
    self.tokenizer_base.end()
  }

  fn reset(&mut self) -> Result<()> {
    self.tokenizer_base.reset()?;
    self.upto = 0;
    let mut text = String::new();
    let mut buffer = ['\0'; 1024];
    loop {
      let count = self.tokenizer_base.input.read_range(&mut buffer, 0, 1024)?;
      if count == -1 {
        break;
      }
      for ch in &buffer[..count as usize] {
        text.push(*ch);
      }
    }
    self.tokens = text.split(' ').map(str::to_string).collect();
    if !text.is_empty() {
      while self.tokens.last().is_some_and(String::is_empty) {
        self.tokens.pop();
      }
    }
    Ok(())
  }

  fn get_attribute_source(&self) -> &Attributes {
    self.tokenizer_base.get_attribute_source()
  }

  fn get_attribute_source_mut(&mut self) -> &mut Attributes {
    self.tokenizer_base.get_attribute_source_mut()
  }

  fn set_reader(&mut self, input: crate::core::analysis::reader::ReaderEnum) -> Result<()> {
    self.tokenizer_base.set_reader(input)
  }
}

impl Tokenizer for StringSplitTokenizer {
  fn get_tokenizer_base_mut(&mut self) -> &mut TokenizerBase {
    &mut self.tokenizer_base
  }

  fn get_tokenizer_base(&self) -> &TokenizerBase {
    &self.tokenizer_base
  }
}

#[test]
fn test_wicked_long_term() -> Result<()> {
  // Make sure we skip wicked long terms.
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = StringSplitAnalyzer {
    stored_value: AnalyzerStoredValue::new(),
  };
  let w = RandomIndexWriter::with_analyzer(
    &mut random,
    dir.clone(),
    Box::new(analyzer) as Box<dyn Analyzer>,
  )?;

  let big_term = "x".repeat(MAX_TERM_LENGTH as usize);
  let mut huge_doc = Document::new();

  // This contents produces a too-long term:
  let contents = format!("abc xyz x{big_term} another term");
  huge_doc.add(TextField::from_string("content", contents, Store::No)?);
  let err = w.add_document(&mut random, huge_doc);
  assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));

  // Make sure we can add another normal document
  let mut doc = Document::new();
  doc.add(TextField::from_string("content", "abc bbb ccc", Store::No)?);
  w.add_document(&mut random, doc)?;

  // So we remove the deleted doc:
  w.w.force_merge(1)?;

  let reader = w.get_reader(&mut random)?;
  w.close(&mut random)?;

  // Make sure all terms < max size were indexed
  assert_eq!(1, reader.doc_freq(&Term::from_text("content", "abc"))?);
  assert_eq!(1, reader.doc_freq(&Term::from_text("content", "bbb"))?);
  assert_eq!(0, reader.doc_freq(&Term::from_text("content", "term"))?);

  // Make sure the doc that has the massive term is NOT in the index:
  assert_eq!(
    1,
    reader.num_docs()?,
    "document with wicked long term is in the index!"
  );

  reader.close()?;
  let dir = new_directory_shared(&mut random)?;

  // Make sure we can add a document with exactly the maximum length term,
  // and search on that term:
  let mut custom_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  custom_type.set_tokenized(false)?;

  let mut iwc = new_index_writer_config(&mut random)?;
  iwc.set_codec(TestUtil::get_default_codec());
  let w2 = RandomIndexWriter::with_config(&mut random, dir, iwc);

  let mut doc = Document::new();
  doc.add(Field::from_string("content", "other", custom_type.clone())?);
  w2.add_document(&mut random, doc)?;

  let mut doc = Document::new();
  doc.add(Field::from_string("content", "term", custom_type.clone())?);
  w2.add_document(&mut random, doc)?;

  let mut doc = Document::new();
  doc.add(Field::from_string(
    "content",
    big_term.clone(),
    custom_type.clone(),
  )?);
  w2.add_document(&mut random, doc)?;

  let mut doc = Document::new();
  doc.add(Field::from_string("content", "zzz", custom_type)?);
  w2.add_document(&mut random, doc)?;

  let reader = w2.get_reader(&mut random)?;
  w2.close(&mut random)?;
  assert_eq!(1, reader.doc_freq(&Term::from_text("content", big_term))?);

  reader.close()?;
  Ok(())
}
#[test]
fn test_delete_all_nrt_leftover_files() -> Result<()> {
  let mut random = random();

  let dir = Arc::new(ByteBuffersDirectory::new());

  let mock = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(mock)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let doc = Document::new();

  for _ in 0..20 {
    for _ in 0..100 {
      w.add_document(doc.clone())?;
    }

    w.commit()?;

    let reader = directory_reader::open_from_writer(&w)?;
    reader.close()?;

    w.delete_all()?;
    w.commit()?;

    // Make sure we accumulate no files except for empty segments_N and segments.gen.
    let files = dir.list_all()?;
    assert!(files.len() <= 2, "unexpected leftover files: {files:?}");
  }

  w.close()?;

  Ok(())
}
#[test]
fn test_nrt_reader_version() -> Result<()> {
  let mut random = random();

  let dir = Arc::new(ByteBuffersDirectory::new());

  let mock = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(mock)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let mut field_types = HashMap::new();

  let mut doc = Document::new();
  doc.add(new_string_field(
    &mut random,
    "id",
    "0",
    Store::Yes,
    &mut field_types,
  )?);

  w.add_document(doc.clone())?;

  let r = directory_reader::open_from_writer(&w)?;
  let version = r.get_version()?;
  drop(r);

  w.add_document(doc.clone())?;

  let r = directory_reader::open_from_writer(&w)?;
  let version2 = r.get_version()?;
  drop(r);

  assert!(version2 > version);

  w.delete_documents_with_terms(vec![Term::from_text("id", "0")])?;

  let r = directory_reader::open_from_writer(&w)?;
  w.close()?;

  let version3 = r.get_version()?;
  drop(r);

  assert!(version3 > version2);
  Ok(())
}

#[test]
fn test_whether_delete_all_deletes_write_lock() -> Result<()> {
  let mut random = random();
  // Must use SimpleFSLockFactory...
  // NativeFSLockFactory somehow "knows" a lock is held against write.lock
  // even if you remove that file:
  let temp_dir = create_temp_dir()?;
  let dir = Arc::new(new_fs_directory_with_lock_factory(
    &mut random,
    temp_dir.keep(),
    SimpleFSLockFactory::new(),
  )?);
  let w1 = RandomIndexWriter::new(&mut random, dir.clone())?;
  w1.delete_all()?;

  assert!(matches!(
    IndexWriter::new(dir.clone(), new_index_writer_config(&mut random)?),
    Err(LuceneError::LockObtainFailed(_))
  ));

  w1.close(&mut random)?;
  Ok(())
}

#[test]
fn test_has_blocks_merge_fully_del_segments() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;

  let mock = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(mock)?;
  let writer = IndexWriter::new(dir.clone(), iwc)?;

  let new_doc = || -> Result<Document> {
    let mut doc = Document::new();
    doc.add(StringField::from_string("foo", "bar", Store::No)?);
    Ok(doc)
  };

  let docs = vec![new_doc()?, new_doc()?];
  writer.update_documents_with_term(Term::from_text("foo", "bar"), docs.clone())?;
  writer.commit()?;

  if random.random_bool(0.5) {
    writer.update_documents_with_term(Term::from_text("foo", "bar"), docs)?;
    writer.commit()?;
  }

  writer.update_document_with_term(Term::from_text("foo", "bar"), new_doc()?)?;

  if random.random_bool(0.5) {
    writer.force_merge_deletes_with_wait(true)?;
  } else {
    writer.force_merge_with_wait(1, true)?;
  }

  writer.commit()?;

  let reader = directory_reader::open(dir.clone())?;
  let reader = reader.get_context()?;
  let leaves = reader.leaves()?;
  assert_eq!(1, leaves.len());

  assert!(
    !leaves[0].reader().get_metadata()?.get_has_blocks(),
    "hasBlocks should be cleared"
  );

  writer.close()?;

  Ok(())
}

#[test]
fn test_single_docs_do_not_trigger_has_blocks() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;

  let mock = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(mock)?;
  iwc.set_max_buffered_docs(i32::MAX);
  iwc.set_ram_buffer_size_mb(100.0);

  let w = IndexWriter::new(dir.clone(), iwc)?;

  let docs = TestUtil::next_int(&mut random, 1, 100);
  for i in 0..docs {
    let mut doc = Document::new();
    doc.add(StringField::from_string("id", i.to_string(), Store::No)?);
    w.add_documents(vec![doc])?;
  }

  w.commit()?;

  let si = w.clone_segment_infos()?;
  assert_eq!(1, si.size());
  assert!(!si.iter()[0].info.get_has_blocks());

  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "XXX", Store::No)?);

  w.add_documents(vec![doc.clone(), doc])?;
  w.commit()?;

  let si = w.clone_segment_infos()?;
  assert_eq!(2, si.size());

  let infos = si.iter();
  assert!(!infos[0].info.get_has_blocks());
  assert!(infos[1].info.get_has_blocks());

  w.force_merge(1)?;
  w.commit()?;

  let si = w.clone_segment_infos()?;
  assert_eq!(1, si.size());
  assert!(si.iter()[0].info.get_has_blocks());

  w.close()?;

  Ok(())
}

#[test]
fn test_carry_over_has_blocks() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;

  let mock = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(mock)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let mut docs = vec![Document::new()];
  w.update_documents_with_term(Term::from_text("foo", "bar"), docs.clone())?;
  w.commit()?;

  {
    let reader = directory_reader::open(dir.clone())?;
    let reader = reader.get_context()?;
    let leaves = reader.leaves()?;
    let segment_info = leaves[0].reader().get_segment_info();
    assert!(!segment_info.info.get_has_blocks());
  }

  docs.push(Document::new());

  w.update_documents_with_term(Term::from_text("foo", "bar"), docs)?;
  w.commit()?;

  {
    let reader = directory_reader::open(dir.clone())?;
    let reader = reader.get_context()?;
    let leaves = reader.leaves()?;
    assert_eq!(2, leaves.len());

    let segment_info = leaves[0].reader().get_segment_info();
    assert!(!segment_info.info.get_has_blocks(),);

    let segment_info = leaves[1].reader().get_segment_info();
    assert!(segment_info.info.get_has_blocks(),);
  }

  w.force_merge_with_wait(1, true)?;
  w.commit()?;

  {
    let reader = directory_reader::open(dir.clone())?;
    let reader = reader.get_context()?;
    let leaves = reader.leaves()?;
    assert_eq!(1, leaves.len());

    let segment_info = leaves[0].reader().get_segment_info();
    assert!(segment_info.info.get_has_blocks(),);
  }

  w.commit()?;
  w.close()?;

  Ok(())
}

#[test]
fn test_prepare_commit_then_close() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;

  let mock = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(mock)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  w.prepare_commit()?;

  let err = w.close();
  assert!(matches!(err, Err(LuceneError::IllegalState(_))));

  w.commit()?;
  w.close()?;
  drop(w);

  let r = directory_reader::open(dir)?;
  assert_eq!(0, r.max_doc()?);

  Ok(())
}

#[test]
fn test_prepare_commit_then_rollback() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let mock = MockAnalyzer::new(&mut random);
  let conf = IndexWriterConfig::with_analyzer(mock)?;
  let w = IndexWriter::new(dir.clone(), conf)?;

  w.prepare_commit()?;
  w.rollback()?;

  assert!(!directory_reader::index_exists(&dir)?);

  Ok(())
}

#[test]
fn test_prepare_commit_then_rollback2() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let mock = MockAnalyzer::new(&mut random);
  let conf = IndexWriterConfig::with_analyzer(mock)?;
  let w = IndexWriter::new(dir.clone(), conf)?;

  w.commit()?;
  w.add_document(Document::new())?;
  w.prepare_commit()?;
  w.rollback()?;

  assert!(directory_reader::index_exists(&dir)?);

  let r = directory_reader::open(dir.clone())?;
  assert_eq!(0, r.max_doc()?);

  r.close()?;

  Ok(())
}

#[test]
fn test_dont_invoke_analyzer_for_un_analyzed_fields() -> Result<()> {
  let mut random = random();
  let analyzer = DontInvokeAnalyzer {
    stored_value: AnalyzerStoredValue::new(),
  };
  let dir = new_directory_shared(&mut random)?;
  let config =
    new_index_writer_config_with_analyzer(&mut random, Box::new(analyzer) as Box<dyn Analyzer>)?;
  let w = IndexWriter::new(dir, config)?;

  let mut doc = Document::new();
  let mut custom_type =
    FieldType::from_ref(&*crate::core::document::string_field::TYPE_NOT_STORED)?;
  custom_type.set_store_term_vectors(true)?;
  custom_type.set_store_term_vector_positions(true)?;
  custom_type.set_store_term_vector_offsets(true)?;
  let mut field_to_type = HashMap::new();
  let f = new_field(
    &mut random,
    "field",
    "abcd",
    &custom_type,
    &mut field_to_type,
  )?;
  doc.add(f.clone());
  doc.add(f.clone());
  let f2 = new_field(&mut random, "field", "", &custom_type, &mut field_to_type)?;
  doc.add(f2);
  doc.add(f);
  w.add_document(doc)?;
  w.close()?;

  Ok(())
}

struct DontInvokeAnalyzer {
  stored_value: AnalyzerStoredValue,
}

impl Analyzer for DontInvokeAnalyzer {
  fn create_components(&self, _field_name: &str) -> Result<TokenStreamComponents> {
    unreachable!("don't invoke me!")
  }

  fn get_position_increment_gap(&self, _field_name: &str) -> i32 {
    unreachable!("don't invoke me!")
  }

  fn get_offset_gap(&self, _field_name: &str) -> i32 {
    unreachable!("don't invoke me!")
  }

  fn stored_value(&self) -> &AnalyzerStoredValue {
    &self.stored_value
  }
}

crate::impl_analyzer_close!(DontInvokeAnalyzer);

#[test]
fn test_other_files() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;

  let mock = MockAnalyzer::new(&mut random);
  let iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let iw = IndexWriter::new(dir.clone(), iwc)?;
  iw.add_document(Document::new())?;
  iw.close()?;
  drop(iw);

  {
    // Create my own random file.
    let context = new_io_context(&mut random)?;
    let mut out = dir.create_output("myrandomfile", &context)?;
    out.write_byte(42)?;
    out.close()?;
  }

  let mock = MockAnalyzer::new(&mut random);
  let iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let iw = IndexWriter::new(dir.clone(), iwc)?;
  iw.close()?;

  assert!(slow_file_exists(&dir, "myrandomfile")?);

  Ok(())
}

#[test]
fn test_stopwords_pos_inc_hole() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = StopwordsPosIncHoleAnalyzer {
    stored_value: AnalyzerStoredValue::new(),
    seed: random.random(),
  };
  let iw =
    RandomIndexWriter::with_analyzer(&mut random, dir, Box::new(analyzer) as Box<dyn Analyzer>)?;
  let mut doc = Document::new();
  doc.add(TextField::from_string("body", "just a", Store::No)?);
  doc.add(TextField::from_string("body", "test of gaps", Store::No)?);
  iw.add_document(&mut random, doc)?;
  let ir = iw.get_reader(&mut random)?;
  iw.close(&mut random)?;
  let searcher = new_searcher_with_reader(ir)?;
  let mut builder = PhraseQueryBuilder::new();
  builder.add(Term::from_text("body", "just"), 0)?;
  builder.add(Term::from_text("body", "test"), 2)?;
  let pq = builder.build()?;
  assert_eq!(1, searcher.search(pq, 5)?.total_hits.value());

  Ok(())
}

#[test]
fn test_stopwords_pos_inc_hole2() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let second_set = Automata::make_string("foobar")?;
  let analyzer = StopwordsPosIncHole2Analyzer {
    stored_value: AnalyzerStoredValue::new(),
    seed: random.random(),
    second_set,
  };
  let iw =
    RandomIndexWriter::with_analyzer(&mut random, dir, Box::new(analyzer) as Box<dyn Analyzer>)?;
  let mut doc = Document::new();
  doc.add(TextField::from_string("body", "just a foobar", Store::No)?);
  doc.add(TextField::from_string("body", "test of gaps", Store::No)?);
  iw.add_document(&mut random, doc)?;
  let ir = iw.get_reader(&mut random)?;
  iw.close(&mut random)?;
  let searcher = new_searcher_with_reader(ir)?;
  let mut builder = PhraseQueryBuilder::new();
  builder.add(Term::from_text("body", "just"), 0)?;
  builder.add(Term::from_text("body", "test"), 3)?;
  let pq = builder.build()?;
  assert_eq!(1, searcher.search(pq, 5)?.total_hits.value());

  Ok(())
}

struct StopwordsPosIncHoleAnalyzer {
  stored_value: AnalyzerStoredValue,
  seed: u64,
}

impl Analyzer for StopwordsPosIncHoleAnalyzer {
  fn create_components(&self, _field_name: &str) -> Result<TokenStreamComponents> {
    let tokenizer = MockTokenizer::new(random_from_seed(self.seed));
    let stream = MockTokenFilter::new(tokenizer, ENGLISH_STOPSET.clone());
    Ok(TokenStreamComponents::new(
      Box::new(stream) as Box<dyn TokenStream + Send + Sync>,
      None,
    ))
  }

  fn stored_value(&self) -> &AnalyzerStoredValue {
    &self.stored_value
  }
}

crate::impl_analyzer_close!(StopwordsPosIncHoleAnalyzer);

struct StopwordsPosIncHole2Analyzer {
  stored_value: AnalyzerStoredValue,
  seed: u64,
  second_set: Automaton,
}

impl Analyzer for StopwordsPosIncHole2Analyzer {
  fn create_components(&self, _field_name: &str) -> Result<TokenStreamComponents> {
    let tokenizer = MockTokenizer::new(random_from_seed(self.seed));
    let stream = MockTokenFilter::new(tokenizer, ENGLISH_STOPSET.clone());
    let stream = MockTokenFilter::new(stream, CharacterRunAutomaton::new(self.second_set.clone())?);
    Ok(TokenStreamComponents::new(
      Box::new(stream) as Box<dyn TokenStream + Send + Sync>,
      None,
    ))
  }

  fn stored_value(&self) -> &AnalyzerStoredValue {
    &self.stored_value
  }
}

crate::impl_analyzer_close!(StopwordsPosIncHole2Analyzer);

#[test]
fn test_commit_with_user_data_only() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;

  let iwc = new_index_writer_config(&mut random)?;
  let writer = IndexWriter::new(dir.clone(), iwc)?;

  writer.commit()?; // first commit to complete IW create transaction.

  // This should store the commit data, even though no other changes were made.
  let mut data = HashMap::new();
  data.insert("key".to_string(), "value".to_string());
  writer.set_live_commit_data(data);
  writer.commit()?;

  let r = directory_reader::open(dir.clone())?;
  assert_eq!(
    Some(&"value".to_string()),
    r.get_index_commit()?.get_user_data().get("key")
  );

  // Now check setCommitData and prepareCommit/commit sequence.
  let mut data = HashMap::new();
  data.insert("key".to_string(), "value1".to_string());
  writer.set_live_commit_data(data);

  writer.prepare_commit()?;

  let mut data = HashMap::new();
  data.insert("key".to_string(), "value2".to_string());
  writer.set_live_commit_data(data);

  // Should commit the first commitData only, per protocol.
  writer.commit()?;

  let r = directory_reader::open(dir.clone())?;
  assert_eq!(
    Some(&"value1".to_string()),
    r.get_index_commit()?.get_user_data().get("key")
  );

  // Now should commit the second commitData - there was a bug where
  // IndexWriter.finishCommit overrode the second commitData.
  writer.commit()?;

  let r = directory_reader::open(dir.clone())?;
  assert_eq!(
    Some(&"value2".to_string()),
    r.get_index_commit()?.get_user_data().get("key"),
    "IndexWriter.finishCommit may have overridden the second commitData"
  );

  writer.close()?;

  Ok(())
}

fn get_live_commit_data<D>(writer: &IndexWriter<D>) -> HashMap<String, String>
where
  D: Directory,
{
  let mut data = HashMap::new();

  if let Some(iter) = writer.get_live_commit_data() {
    for ent in iter {
      data.insert(ent.0.clone(), ent.1.clone());
    }
  }

  data
}

#[test]
fn test_get_commit_data() -> Result<()> {
  let dir = new_directory_shared(&mut random())?;
  let mut random = random();

  let iwc = new_index_writer_config(&mut random)?;
  let writer = IndexWriter::new(dir.clone(), iwc)?;

  writer.set_live_commit_data(HashMap::from([("key".to_string(), "value".to_string())]));

  assert_eq!(
    Some("value"),
    get_live_commit_data(&writer).get("key").map(String::as_str)
  );

  writer.close()?;
  drop(writer);

  // Validate that it's also visible when opening a new IndexWriter.
  let mut iwc = new_index_writer_config::<DirEnum, _>(&mut random)?;
  iwc.set_open_mode(OpenMode::Append);

  let writer = IndexWriter::new(dir.clone(), iwc)?;

  assert_eq!(
    Some("value"),
    get_live_commit_data(&writer).get("key").map(String::as_str)
  );

  writer.close()?;

  Ok(())
}

#[test]
fn test_get_commit_data_from_old_snapshot() -> Result<()> {
  let dir = new_directory_shared(&mut random())?;
  let mut random = random();

  let iwc = new_snapshot_index_writer_config(&mut random)?;
  let writer = IndexWriter::new(dir.clone(), iwc)?;

  let mut data = HashMap::new();
  data.insert("key".to_string(), "value".to_string());
  writer.set_live_commit_data(data);
  assert_eq!(
    Some("value"),
    get_live_commit_data(&writer).get("key").map(String::as_str)
  );
  writer.commit()?;
  // Snapshot this commit to open later.
  let index_commit = match writer.get_config().get_index_deletion_policy() {
    IndexDeletionPolicyEnum::Snapshot(policy) => policy.snapshot()?,
    policy => {
      return Err(LuceneError::illegal_state(format!(
        "expected SnapshotDeletionPolicy but got {policy}"
      )));
    },
  };
  writer.close()?;
  drop(writer);

  // Modify the commit data and commit on close so the most recent commit data is different.
  let iwc = new_snapshot_index_writer_config(&mut random)?;
  let writer = IndexWriter::new(dir.clone(), iwc)?;
  let mut data = HashMap::new();
  data.insert("key".to_string(), "value2".to_string());
  writer.set_live_commit_data(data);
  assert_eq!(
    Some("value2"),
    get_live_commit_data(&writer).get("key").map(String::as_str)
  );
  writer.close()?;
  drop(writer);

  // Validate that when opening writer from older snapshotted index commit,
  // the old commit data is visible.
  let mut iwc = new_snapshot_index_writer_config(&mut random)?;
  iwc.set_open_mode(OpenMode::Append);
  let index_commit_wrapper =
    IndexCommitWrapper::<Arc<CommitPoint<DirEnum>>, DirEnum>::new(Some(index_commit), None)?;
  let writer = IndexWriter::with_index_commit(dir.clone(), iwc, index_commit_wrapper)?;
  assert_eq!(
    Some("value"),
    get_live_commit_data(&writer).get("key").map(String::as_str)
  );
  writer.close()?;

  Ok(())
}

#[test]
#[ignore = "Java-only: Rust IndexWriterConfig requires a concrete analyzer"]
fn test_null_analyzer() -> Result<()> {
  test_not_required_in_rust_lucene!();
}

#[test]
#[ignore = "Java-only: Rust IndexWriter::add_document requires a concrete Document"]
fn test_null_document() -> Result<()> {
  test_not_required_in_rust_lucene!();
}

#[test]
#[ignore = "Java-only: Rust document iterators cannot yield Java null Documents"]
fn test_null_documents() -> Result<()> {
  test_not_required_in_rust_lucene!();
}

#[test]
fn test_iterable_field_throws_exception() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let writer = IndexWriter::new(
    dir,
    new_index_writer_config_with_analyzer(&mut random, analyzer)?,
  )?;
  let iters = at_least(&mut random, 100);
  let mut doc_count = 0;
  let mut doc_id = 0;
  let mut live_ids = HashSet::new();
  for _ in 0..iters {
    let num_docs = at_least(&mut random, 4);
    for _ in 0..num_docs {
      let id = doc_id.to_string();
      doc_id += 1;
      let fields = vec![
        StringField::from_string("id", id.clone(), Store::Yes)?.into(),
        StringField::from_string(
          "foo",
          TestUtil::random_simple_string(&mut random),
          Store::No,
        )?
        .into(),
      ];
      doc_id += 1;

      match writer.add_document(RandomFailingIterable::new(fields, &mut random)) {
        Ok(_) => {
          doc_count += 1;
          live_ids.insert(id);
        },
        Err(error @ LuceneError::IllegalState(_)) => {
          assert_eq!("boom", error.to_string());
        },
        Err(error) => return Err(error),
      }
    }
  }
  let reader = directory_reader::open_from_writer(&writer)?;
  assert_eq!(doc_count, reader.num_docs()?);
  let context = (&reader).get_context()?;
  for leaf_reader_context in context.leaves()? {
    let leaf_reader = leaf_reader_context.reader();
    let live_docs = leaf_reader.get_live_docs()?;
    let max_doc = leaf_reader.max_doc()?;
    let mut stored_fields = leaf_reader.stored_fields()?;
    for i in 0..max_doc {
      let is_live = match &live_docs {
        Some(live_docs) => live_docs.get(i as usize)?,
        None => true,
      };
      if is_live {
        let document = stored_fields.document(i)?;
        let id = document
          .get("id")?
          .ok_or_else(|| LuceneError::illegal_state("missing id field"))?;
        assert!(live_ids.remove(id.as_str()));
      }
    }
  }
  assert!(live_ids.is_empty());
  writer.close()?;
  reader.close()?;
  Ok(())
}

#[test]
fn test_iterable_throws_exception() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let writer = IndexWriter::new(
    dir,
    new_index_writer_config_with_analyzer(&mut random, analyzer)?,
  )?;
  let iters = at_least(&mut random, 100);
  let mut doc_count = 0;
  let mut doc_id = 0;
  let mut live_ids = HashSet::new();
  for _ in 0..iters {
    let num_docs = at_least(&mut random, 4);
    for _ in 0..num_docs {
      let id = doc_id.to_string();
      doc_id += 1;
      let fields = vec![
        StringField::from_string("id", id.clone(), Store::Yes)?.into(),
        StringField::from_string(
          "foo",
          TestUtil::random_simple_string(&mut random),
          Store::No,
        )?
        .into(),
      ];
      doc_id += 1;

      match writer.add_document(RandomFailingIterable::new(fields, &mut random)) {
        Ok(_) => {
          doc_count += 1;
          live_ids.insert(id);
        },
        Err(error @ LuceneError::IllegalState(_)) => {
          assert_eq!("boom", error.to_string());
        },
        Err(error) => return Err(error),
      }
    }
  }
  let reader = directory_reader::open_from_writer(&writer)?;
  assert_eq!(doc_count, reader.num_docs()?);
  let context = (&reader).get_context()?;
  for leaf_reader_context in context.leaves()? {
    let leaf_reader = leaf_reader_context.reader();
    let live_docs = leaf_reader.get_live_docs()?;
    let max_doc = leaf_reader.max_doc()?;
    let mut stored_fields = leaf_reader.stored_fields()?;
    for i in 0..max_doc {
      let is_live = match &live_docs {
        Some(live_docs) => live_docs.get(i as usize)?,
        None => true,
      };
      if is_live {
        let document = stored_fields.document(i)?;
        let id = document
          .get("id")?
          .ok_or_else(|| LuceneError::illegal_state("missing id field"))?;
        assert!(live_ids.remove(id.as_str()));
      }
    }
  }
  assert!(live_ids.is_empty());
  writer.close()?;
  reader.close()?;
  Ok(())
}

#[test]
fn test_iterable_throws_exception2() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let writer = IndexWriter::new(
    dir,
    new_index_writer_config_with_analyzer(&mut random, analyzer)?,
  )?;
  let error = writer
    .add_documents(FailingDocumentsIterable)
    .expect_err("iterator should fail");
  assert_eq!("boom", error.to_string());
  writer.close()?;
  Ok(())
}

struct FailingDocumentsIterable;

struct FailingDocumentsIterator;

impl IntoFallibleIterator for FailingDocumentsIterable {
  type Item = Document;
  type IntoIter = FailingDocumentsIterator;

  fn into_fallible_iter(self) -> Self::IntoIter {
    FailingDocumentsIterator
  }
}

impl Iterator for FailingDocumentsIterator {
  type Item = Result<Document>;

  fn next(&mut self) -> Option<Self::Item> {
    Some(Err(LuceneError::illegal_state("boom")))
  }
}

struct RandomFailingIterable<T> {
  list: Vec<T>,
  fail_on: usize,
}

impl<T> RandomFailingIterable<T> {
  fn new<R>(list: Vec<T>, random: &mut R) -> Self
  where
    R: Rng + ?Sized,
  {
    Self {
      list,
      fail_on: random.random_range(0..5),
    }
  }
}

struct RandomFailingIterator<T> {
  iterator: std::vec::IntoIter<T>,
  fail_on: usize,
  count: usize,
}

impl<T> IntoFallibleIterator for RandomFailingIterable<T> {
  type Item = T;
  type IntoIter = RandomFailingIterator<T>;

  fn into_fallible_iter(self) -> Self::IntoIter {
    RandomFailingIterator {
      iterator: self.list.into_iter(),
      fail_on: self.fail_on,
      count: 0,
    }
  }
}

impl<T> Iterator for RandomFailingIterator<T> {
  type Item = Result<T>;

  fn next(&mut self) -> Option<Self::Item> {
    if self.iterator.len() == 0 {
      return None;
    }
    if self.count == self.fail_on {
      return Some(Err(LuceneError::illegal_state("boom")));
    }
    self.count += 1;
    self.iterator.next().map(Ok)
  }
}

#[test]
fn test_corrupt_first_commit() -> Result<()> {
  // LUCENE-2727/LUCENE-2812/LUCENE-4738:
  let mut random = random();
  for i in 0..6 {
    let dir = new_directory_shared(&mut random)?;

    // Create a corrupt first commit:
    let pending_segments =
      IndexFileNames::file_name_from_generation(IndexFileNames::PENDING_SEGMENTS, "", 0)
        .expect("generation 0 should produce a file name");
    dir
      .create_output(&pending_segments, &IOContext::default_io_context()?)?
      .close()?;

    let mock = MockAnalyzer::new(&mut random);
    let mut iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
    let mode = i / 2;
    if mode == 0 {
      iwc.set_open_mode(OpenMode::Create);
    } else if mode == 1 {
      iwc.set_open_mode(OpenMode::Append);
    } else if mode == 2 {
      iwc.set_open_mode(OpenMode::CreateOrAppend);
    }

    let result = (|| -> Result<()> {
      let writer = IndexWriter::new(dir.clone(), iwc)?;
      if (i & 1) == 0 {
        writer.close()
      } else {
        writer.rollback()
      }
    })();

    if let Err(error) = result {
      // OpenMode.APPEND should throw an exception since no index exists:
      if mode == 0 {
        return Err(error);
      }
    }
  }
  Ok(())
}

#[test]
fn test_has_uncommitted_changes() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  iwc.set_merge_policy(NoMergePolicy::default());
  let writer = IndexWriter::new(dir.clone(), iwc)?;

  assert!(writer.has_uncommitted_changes()?);

  let mut doc = Document::new();
  doc.add(TextField::from_string("myfield", "a b c", Store::No)?);
  writer.add_document(doc.clone())?;
  assert!(writer.has_uncommitted_changes()?);

  writer.commit()?;
  writer.wait_for_merges()?;
  writer.commit()?;
  assert!(!writer.has_uncommitted_changes()?);

  writer.add_document(doc)?;
  assert!(writer.has_uncommitted_changes()?);
  writer.commit()?;

  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "xyz", Store::Yes)?);
  writer.add_document(doc.clone())?;
  assert!(writer.has_uncommitted_changes()?);

  writer.commit()?;
  assert!(!writer.has_uncommitted_changes()?);
  writer.delete_documents_with_terms(vec![Term::from_text("id", "xyz")])?;
  assert!(writer.has_uncommitted_changes()?);

  writer.commit()?;
  assert!(!writer.has_uncommitted_changes()?);
  writer.close()?;
  drop(writer);

  let mock = MockAnalyzer::new(&mut random);
  let iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let writer = IndexWriter::new(dir.clone(), iwc)?;
  assert!(!writer.has_uncommitted_changes()?);
  writer.add_document(doc)?;
  assert!(writer.has_uncommitted_changes()?);

  writer.close()?;

  Ok(())
}

struct MergeAllDeletedTestPoint {
  keep_fully_deleted_segments: Arc<AtomicBool>,
}

impl TestPoint for MergeAllDeletedTestPoint {
  fn apply(&self, message: &str) -> Result<()> {
    if message == "startCommitMerge" {
      self.keep_fully_deleted_segments.store(false, SeqCst);
    } else if message == "startMergeInit" {
      self.keep_fully_deleted_segments.store(true, SeqCst);
    }
    Ok(())
  }
}

#[test]
fn test_merge_all_deleted() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let keep_fully_deleted_segments = Arc::new(AtomicBool::new(false));
  iwc.set_merge_policy(
    KeepFullyDeletedSegmentsMergePolicy::with_keep_fully_deleted_segments(
      keep_fully_deleted_segments.clone(),
    ),
  );
  let evil_writer = RandomIndexWriter::mock_index_writer_with_test_point(
    &mut random,
    dir,
    iwc,
    MergeAllDeletedTestPoint {
      keep_fully_deleted_segments,
    },
  )?;
  let mut field_types = HashMap::new();
  for _ in 0..1000 {
    add_doc(&mut random, &evil_writer, &mut field_types)?;
    if random.random_range(0..17) == 0 {
      evil_writer.commit()?;
    }
  }
  evil_writer.delete_documents_with_queries(vec![MatchAllDocsQuery::new().into()])?;
  evil_writer.force_merge(1)?;
  evil_writer.close()?;
  Ok(())
}

#[test]
fn test_delete_same_term_across_fields() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let mock = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(mock)?;
  let writer = IndexWriter::new(dir.clone(), iwc)?;

  let mut doc = Document::new();
  doc.add(TextField::from_string("a", "foo", Store::No)?);
  writer.add_document(doc)?;

  writer.delete_documents_with_terms(vec![
    Term::from_text("a", "xxx"),
    Term::from_text("b", "foo"),
  ])?;

  let reader = directory_reader::open_from_writer(&writer)?;
  writer.close()?;
  assert_eq!(1, reader.num_docs()?);

  Ok(())
}

#[test]
fn test_has_uncommitted_changes_after_exception() -> Result<()> {
  let mut random = random();
  let analyzer = MockAnalyzer::new(&mut random);

  let directory = new_directory_shared(&mut random)?;
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  iwc.set_merge_policy(new_log_merge_policy(&mut random)?);
  let iwriter = IndexWriter::new(directory, iwc)?;

  let mut doc = Document::new();
  doc.add(SortedDocValuesField::new(
    "dv",
    BytesRef::from_string("foo!"),
  ));
  doc.add(SortedDocValuesField::new(
    "dv",
    BytesRef::from_string("bar!"),
  ));
  let result = iwriter.add_document(doc);
  assert!(matches!(result, Err(LuceneError::IllegalArgument(_))));

  iwriter.commit()?;
  assert!(!iwriter.has_uncommitted_changes()?);
  iwriter.close()?;

  Ok(())
}

#[test]
fn test_double_close() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let w = IndexWriter::new(
    dir,
    new_index_writer_config_with_analyzer(&mut random, analyzer)?,
  )?;

  let mut doc = Document::new();
  doc.add(SortedDocValuesField::new(
    "dv",
    BytesRef::from_string("foo!"),
  ));
  w.add_document(doc)?;
  w.close()?;
  w.close()?;
  Ok(())
}

#[test]
fn test_rollback_then_close() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let w = IndexWriter::new(
    dir,
    new_index_writer_config_with_analyzer(&mut random, analyzer)?,
  )?;

  let mut doc = Document::new();
  doc.add(SortedDocValuesField::new(
    "dv",
    BytesRef::from_string("foo!"),
  ));
  w.add_document(doc)?;
  w.rollback()?;
  // Close after rollback should have no effect
  w.close()?;

  Ok(())
}

#[test]
fn test_close_then_rollback() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let w = IndexWriter::new(
    dir,
    new_index_writer_config_with_analyzer(&mut random, analyzer)?,
  )?;

  let mut doc = Document::new();
  doc.add(SortedDocValuesField::new(
    "dv",
    BytesRef::from_string("foo!"),
  ));
  w.add_document(doc)?;
  w.close()?;
  // Rollback after close should have no effect
  w.rollback()?;

  Ok(())
}

#[test]
fn test_close_while_merge_is_running() -> Result<()> {
  struct CloseWhileMergeIsRunningInfoStream {
    close_started: ConcurrentMergeSchedulerCountDownLatch,
  }

  impl CloseableRef for CloseWhileMergeIsRunningInfoStream {
    fn close(&self) -> Result<()> {
      Ok(())
    }
  }

  impl InfoStream for CloseWhileMergeIsRunningInfoStream {
    fn message(&self, _component: &str, message: &str) -> Result<()> {
      if message == "rollback" {
        self.close_started.count_down();
      }
      Ok(())
    }

    fn is_enabled(&self, _component: &str) -> bool {
      true
    }
  }

  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let merge_started = ConcurrentMergeSchedulerCountDownLatch::new(1);
  let close_started = ConcurrentMergeSchedulerCountDownLatch::new(1);

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  iwc.set_commit_on_close(false);
  let mut mp = LogMergePolicy::<LogDocMergePolicy>::log_doc();
  mp.set_merge_factor(2)?;
  iwc.set_merge_policy(mp);
  iwc.set_info_stream(InfoStreamEnum::Custom(Box::new(
    CloseWhileMergeIsRunningInfoStream {
      close_started: close_started.clone(),
    },
  )));
  iwc.set_merge_scheduler(ConcurrentMergeScheduler::with_hook(
    ConcurrentMergeSchedulerHook::CloseWhileMergeIsRunning(
      CloseWhileMergeIsRunningConcurrentMergeScheduler::new(merge_started, close_started),
    ),
  ));
  let writer = IndexWriter::new(dir.clone(), iwc)?;
  let mut doc = Document::new();
  doc.add(SortedDocValuesField::new(
    "dv",
    BytesRef::from_string("foo!"),
  ));
  writer.add_document(doc.clone())?;
  writer.commit()?;
  writer.add_document(doc)?;
  writer.commit()?;
  writer.close()?;
  dir.as_ref().close()?;
  Ok(())
}

#[test]
fn test_close_during_commit() -> Result<()> {
  // Make sure that close waits for any still-running commits.
  let mut random = random();
  let (start_commit_sender, start_commit_receiver) = mpsc::channel();
  let (finish_commit_sender, finish_commit_receiver) = mpsc::channel();

  let dir = new_directory_shared(&mut random)?;
  let iwc = IndexWriterConfig::new()?;
  // use an InfoStream that "takes a long time" to commit
  let iw = Arc::new(RandomIndexWriter::mock_index_writer_with_test_point(
    &mut random,
    dir,
    iwc,
    CloseDuringCommitTestPoint {
      start_commit: start_commit_sender,
    },
  )?);
  let commit_writer = iw.clone();
  let commit_thread = thread::spawn(move || -> Result<()> {
    commit_writer.commit()?;
    let _ = finish_commit_sender.send(());
    Ok(())
  });

  start_commit_receiver
    .recv()
    .expect("commit should reach finishStartCommit");
  let close_result = iw.close();
  finish_commit_receiver
    .recv()
    .expect("commit thread should finish");
  commit_thread
    .join()
    .expect("commit thread should not panic")?;
  if let Err(error) = close_result
    && !matches!(error, LuceneError::IllegalState(_))
  {
    return Err(error);
  }
  iw.close()?;
  Ok(())
}

struct CloseDuringCommitTestPoint {
  start_commit: mpsc::Sender<()>,
}

impl TestPoint for CloseDuringCommitTestPoint {
  fn apply(&self, message: &str) -> Result<()> {
    if message == "finishStartCommit" {
      let _ = self.start_commit.send(());
      thread::sleep(std::time::Duration::from_millis(10));
    }
    Ok(())
  }
}

#[test]
fn test_ids() -> Result<()> {
  let mut random = random();
  let d = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let w = IndexWriter::new(
    d.clone(),
    new_index_writer_config_with_analyzer(&mut random, analyzer)?,
  )?;
  w.add_document(Document::new())?;
  w.close()?;

  let sis = SegmentInfos::read_latest_commit(d.clone())?;
  let id1 = sis
    .get_id()
    .ok_or_else(|| LuceneError::illegal_state("missing segment infos id"))?;
  assert_eq!(StringHelper::ID_LENGTH, id1.len());

  let id2 = sis.info(0).unwrap().info.get_id();
  let sci_id2 = sis
    .info(0)
    .unwrap()
    .get_id()
    .ok_or_else(|| LuceneError::illegal_state("missing segment commit info id"))?;
  assert_eq!(StringHelper::ID_LENGTH, id2.len());
  assert_eq!(StringHelper::ID_LENGTH, sci_id2.len());

  // Make sure CheckIndex includes id output:
  let mut output = Vec::with_capacity(1024);
  let mut checker = CheckIndex::<_, _, &mut Vec<u8>>::new(d.clone())?;
  checker.set_level(Level::MIN_LEVEL_FOR_INTEGRITY_CHECKS)?;
  checker.set_info_stream_with_verbose(&mut output, false);
  let index_status = checker.check_index()?;
  checker.close()?;
  drop(checker);
  let output = String::from_utf8(output)?;
  // Make sure CheckIndex didn't fail
  assert!(index_status.clean, "{output}");

  // Commit id is always stored:
  let id1 = StringHelper::id_to_string(Some(id1));
  assert!(
    output.contains(&format!("id={id1}")),
    "missing id={id1} in:\n{output}"
  );
  assert!(
    output.contains(&format!("id={id1}")),
    "missing id={id1} in:\n{output}"
  );

  assert_ne!("(null)", id1);

  let mut ids = HashSet::new();
  for i in 0..100000 {
    let id = StringHelper::id_to_string(Some(&StringHelper::random_id()));
    assert!(ids.insert(id.clone()), "id={} i={}", id, i);
  }

  Ok(())
}

#[test]
fn test_empty_norm() -> Result<()> {
  let mut random = random();
  let d = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let w = IndexWriter::new(
    d.clone(),
    new_index_writer_config_with_analyzer(&mut random, analyzer)?,
  )?;
  let mut doc = Document::new();
  doc.add(TextField::from_token_stream(
    "foo",
    FieldTokenStreamEnum::custom(CannedTokenStream::new(Vec::new())),
  )?);
  w.add_document(doc)?;
  w.commit()?;
  w.close()?;

  let r = directory_reader::open(d)?;
  let leaf = get_only_leaf_reader(&r)?;
  let mut norms = LeafReader::get_norm_values(&leaf, "foo")?
    .ok_or_else(|| LuceneError::illegal_state("missing norms for field foo"))?;
  assert_eq!(0, norms.next_doc()?);
  assert_eq!(0, norms.long_value()?);
  r.close()?;

  Ok(())
}

#[test]
fn test_many_separate_threads() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  iwc.set_max_buffered_docs(1000);
  let writer = IndexWriter::new(dir.clone(), iwc)?;

  for _ in 0..100 {
    let writer = writer.clone();
    thread::scope(|scope| -> Result<()> {
      let handle = scope.spawn(move || -> Result<()> {
        let mut doc = Document::new();
        doc.add(StringField::from_string("foo", "bar", Store::No)?);
        writer.add_document(doc)?;
        Ok(())
      });
      handle.join().expect("thread panicked")?;
      Ok(())
    })?;
  }
  writer.close()?;

  let reader = directory_reader::open(dir)?;
  assert_eq!(1, (&reader).get_context()?.leaves()?.len());
  reader.close()?;
  Ok(())
}

#[test]
fn test_nrt_segments_file() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(mock)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;
  // creates segments_1
  w.commit()?;

  // newly opened NRT reader should see gen=1 segments file
  let r = directory_reader::open_from_writer(&w)?;
  let commit = r.get_index_commit()?;
  assert_eq!(1, commit.get_generation());
  assert_eq!("segments_1", commit.get_segments_file_name());

  // newly opened non-NRT reader should see gen=1 segments file
  let r2 = directory_reader::open(dir.clone())?;
  let commit2 = r2.get_index_commit()?;
  assert_eq!(1, commit2.get_generation());
  assert_eq!("segments_1", commit2.get_segments_file_name());
  r2.close()?;

  // make a change and another commit
  let doc = Document::new();
  w.add_document(doc)?;
  w.commit()?;
  let r3 = directory_reader::open_if_changed(&r)?.unwrap();
  r.close()?;

  // reopened NRT reader should see gen=2 segments file
  let commit3 = r3.get_index_commit()?;
  assert_eq!(2, commit3.get_generation());
  assert_eq!("segments_2", commit3.get_segments_file_name());
  r3.close()?;

  // newly opened non-NRT reader should see gen=2 segments file
  let r4 = directory_reader::open(dir.clone())?;
  let commit4 = r4.get_index_commit()?;
  assert_eq!(2, commit4.get_generation());
  assert_eq!("segments_2", commit4.get_segments_file_name());
  r4.close()?;

  w.close()?;
  drop(dir);

  Ok(())
}

#[test]
fn test_nrt_after_commit() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(mock)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;
  w.commit()?;

  let doc = Document::new();
  w.add_document(doc)?;
  let r = directory_reader::open_from_writer(&w)?;
  w.commit()?;

  // commit even with no other changes counts as a "change" that NRT reader reopen will see:
  let r2 = directory_reader::open(dir.clone())?;
  let commit2 = r2.get_index_commit()?;
  assert_eq!(2, commit2.get_generation());
  assert_eq!("segments_2", commit2.get_segments_file_name());

  r2.close()?;
  r.close()?;
  w.close()?;
  drop(dir);

  Ok(())
}

#[test]
fn test_nrt_after_set_user_data_without_commit() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(mock)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;
  w.commit()?;

  let r = directory_reader::open_from_writer(&w)?;
  let mut m = HashMap::new();
  m.insert("foo".to_string(), "bar".to_string());
  w.set_live_commit_data(m);

  // setLiveCommitData with no other changes should count as an NRT change:
  let r2 = directory_reader::open_if_changed(&r)?.unwrap();

  r2.close()?;
  r.close()?;
  w.close()?;
  drop(dir);

  Ok(())
}

#[test]
fn test_nrt_after_set_user_data_with_commit() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(mock)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;
  w.commit()?;

  let r = directory_reader::open_from_writer(&w)?;
  let mut m = HashMap::new();
  m.insert("foo".to_string(), "bar".to_string());
  w.set_live_commit_data(m);
  w.commit()?;
  // setLiveCommitData and also commit, with no other changes, should count as an NRT change:
  let r2 = directory_reader::open_if_changed(&r)?.unwrap();

  r.close()?;
  r2.close()?;
  w.close()?;
  drop(dir);

  Ok(())
}

#[test]
fn test_commit_immediately_after_nrt_reopen() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(mock)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;
  w.commit()?;

  let doc = Document::new();
  w.add_document(doc)?;

  let r = directory_reader::open_from_writer(&w)?;
  w.commit()?;

  assert!(!r.is_current()?);

  let r2 = directory_reader::open_if_changed(&r)?.unwrap();
  // segments_N should have changed:
  assert_ne!(
    r2.get_index_commit()?.get_segments_file_name(),
    r.get_index_commit()?.get_segments_file_name()
  );

  r.close()?;
  r2.close()?;
  w.close()?;
  drop(dir);

  Ok(())
}

#[test]
fn test_pending_delete_dv_generation() -> Result<()> {
  let mut random = random();
  let dir = new_fs_directory(&mut random, create_temp_dir()?)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  iwc.set_use_compound_file(false);
  iwc.set_merge_policy(NoMergePolicy::default());
  iwc.set_max_buffered_docs(2);
  iwc.set_ram_buffer_size_mb(-1.0);
  let mut w = IndexWriter::new(dir.clone(), iwc)?;

  let mut d = Document::new();
  d.add(StringField::from_string("id", "1", Store::Yes)?);
  d.add(NumericDocValuesField::new("nvd", 1));
  w.add_document(d)?;

  let mut d = Document::new();
  d.add(StringField::from_string("id", "2", Store::Yes)?);
  d.add(NumericDocValuesField::new("nvd", 2));
  w.add_document(d)?;
  w.flush()?;

  let mut d = Document::new();
  d.add(StringField::from_string("id", "1", Store::Yes)?);
  d.add(NumericDocValuesField::new("nvd", 1));
  w.update_document_with_term(Term::from_text("id", "1"), d)?;
  w.commit()?;

  let files: HashSet<String> = dir.list_all()?.into_iter().collect();
  let num_iters = 10 + random.random_range(0..50);
  let mut to_close = Vec::new();
  for _ in 0..num_iters {
    if random.random_bool(0.5) {
      let mut d = Document::new();
      d.add(StringField::from_string("id", "1", Store::Yes)?);
      d.add(NumericDocValuesField::new("nvd", 1));
      w.update_document_with_term(Term::from_text("id", "1"), d)?;
    } else if random.random_bool(0.5) {
      w.delete_documents_with_terms(vec![Term::from_text("id", "2")])?;
    } else {
      w.update_numeric_doc_value(Term::from_text("id", "1"), "nvd", 2)?;
    }
    w.prepare_commit()?;
    let mut new_files = dir.list_all()?;
    new_files.retain(|file| !files.contains(file));
    let random_file = new_files[random.random_range(0..new_files.len())].clone();
    to_close.push(dir.open_input(&random_file, &IOContext::default_io_context()?)?);
    w.rollback()?;
    drop(w);

    let analyzer = MockAnalyzer::new(&mut random);
    let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
    iwc.set_use_compound_file(false);
    iwc.set_merge_policy(NoMergePolicy::default());
    iwc.set_max_buffered_docs(2);
    iwc.set_ram_buffer_size_mb(-1.0);
    w = IndexWriter::new(dir.clone(), iwc)?;
    assert!(dir.delete_file(&random_file).is_err());
  }

  drop(to_close);
  w.close()?;

  Ok(())
}

#[test]
fn test_pending_deletions_rollback_with_reader() -> Result<()> {
  let mut random = random();
  let dir = new_fs_directory(&mut random, create_temp_dir()?)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let mut w = IndexWriter::new(dir.clone(), iwc)?;

  let mut d = Document::new();
  d.add(StringField::from_string("id", "1", Store::Yes)?);
  d.add(NumericDocValuesField::new("numval", 1));
  w.add_document(d.clone())?;
  w.commit()?;
  w.add_document(d.clone())?;
  w.flush()?;
  let reader = directory_reader::open_from_writer(&w)?;
  w.rollback()?;
  drop(w);

  // try-delete superfluous files (some will fail due to open readers)
  let analyzer = MockAnalyzer::new(&mut random);
  let iwc2 = IndexWriterConfig::with_analyzer(analyzer)?;
  let writer = IndexWriter::new(dir.clone(), iwc2)?;
  writer.close()?;
  drop(writer);

  // test that we can index on top of pending deletions
  let analyzer = MockAnalyzer::new(&mut random);
  let iwc3 = IndexWriterConfig::with_analyzer(analyzer)?;
  w = IndexWriter::new(dir.clone(), iwc3)?;
  w.add_document(d)?;
  w.commit()?;

  reader.close()?;
  w.close()?;

  Ok(())
}

#[test]
fn test_with_pending_deletions() -> Result<()> {
  // TODO: WindowsFS is not implemented, so pending deletions held by open file handles cannot be
  // exercised.
  test_not_required_in_rust_lucene!();
}

#[test]
fn test_pending_deletes_already_written_files() -> Result<()> {
  // TODO: WindowsFS is not implemented, so already-written pending-delete behavior cannot be
  // exercised.
  test_not_required_in_rust_lucene!();
}

#[test]
fn test_leftover_temp_files() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let writer = IndexWriter::new(dir.clone(), iwc)?;
  writer.close()?;
  drop(writer);

  let io_context = IOContext::default_io_context()?;
  let temp_name = {
    let mut out = dir.create_temp_output("_0", "bkd", &io_context)?;
    let temp_name = out.get_name().to_string();
    out.close()?;
    temp_name
  };

  let analyzer = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let writer = IndexWriter::new(dir.clone(), iwc)?;

  assert!(
    dir.open_input(&temp_name, &io_context).is_err(),
    "did not hit exception"
  );
  writer.close()?;
  Ok(())
}

#[test]
#[ignore = "requires running tests with biggish heap"]
fn test_massive_field() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let writer = IndexWriter::new(dir.clone(), iwc)?;

  let mut b = String::new();
  while b.len() <= MAX_STORED_STRING_LENGTH as usize {
    b.push_str("x ");
  }

  let mut doc = Document::new();
  doc.add(StoredField::from_string("big", b.clone())?);
  let err = writer.add_document(doc);
  assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
  assert_eq!(
    format!(
      "stored field \"big\" is too large ({} characters) to store",
      b.len()
    ),
    err.unwrap_err().to_string()
  );

  let mut doc2 = Document::new();
  doc2.add(StringField::from_string("id", "foo", Store::Yes)?);
  writer.add_document(doc2)?;

  let reader = writer.get_reader(true, true)?;
  assert_eq!(1, reader.num_docs()?);
  reader.close()?;
  writer.close()?;
  Ok(())
}

#[test]
fn test_records_index_created_version() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let writer = IndexWriter::new(dir.clone(), new_index_writer_config(&mut random)?)?;
  writer.commit()?;
  writer.close()?;
  assert_eq!(
    LATEST.major,
    SegmentInfos::read_latest_commit(dir)?.get_index_created_version_major()
  );
  Ok(())
}
#[test]
fn test_flush_largest_writer() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;

  let iwc = IndexWriterConfig::new()?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let num_docs = index_docs_for_multiple_dwpts(&w, &mut random)?;

  let largest_non_pending_writer = w
    .doc_writer
    .flush_control
    .find_largest_non_pending_writer()?
    .unwrap();

  assert!(!largest_non_pending_writer.dwpt.lock().is_flush_pending());

  let num_ram_docs = w.num_ram_docs()?;
  let num_docs_in_dwpt = largest_non_pending_writer.dwpt.lock().get_num_docs_in_ram();

  assert!(w.flush_next_buffer()?);
  assert!(largest_non_pending_writer.dwpt.lock().has_flushed());
  assert_eq!(num_ram_docs - num_docs_in_dwpt, w.num_ram_docs()?);

  // Make sure it's not locked.
  {
    largest_non_pending_writer.lock();
    largest_non_pending_writer.unlock();
  }

  if random.random_bool(0.5) {
    w.commit()?;
  }

  let reader = directory_reader::open_with_writer_deletes(&w, true, true)?;
  assert_eq!(num_docs, reader.num_docs()?);

  w.close()?;

  Ok(())
}

fn index_docs_for_multiple_dwpts<R>(writer: &IndexWriter<DirEnum>, random: &mut R) -> Result<i32>
where
  R: Rng + ?Sized,
{
  let num_threads = 3;
  let latch = Arc::new(Barrier::new(num_threads));
  let num_docs_per_thread = 10 + random.random_range(0..30);

  thread::scope(|scope| -> Result<()> {
    let mut threads = Vec::new();

    for _ in 0..num_threads {
      let latch = latch.clone();

      threads.push(scope.spawn(move || -> Result<()> {
        latch.wait();

        for _ in 0..num_docs_per_thread {
          let mut doc = Document::new();
          doc.add(StringField::from_string("id", "foo", Store::Yes)?);
          writer.add_document(doc)?;
        }

        Ok(())
      }));
    }

    for handle in threads {
      handle.join().expect("thread panicked")?;
    }

    Ok(())
  })?;

  Ok(num_docs_per_thread * num_threads as i32)
}

#[test]
fn test_never_check_out_on_full_flush() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let w = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;

  index_docs_for_multiple_dwpts(&w, &mut random)?;

  let largest_non_pending_writer = w
    .doc_writer
    .flush_control
    .find_largest_non_pending_writer()?
    .unwrap();

  assert!(!largest_non_pending_writer.dwpt.lock().is_flush_pending());
  assert!(!largest_non_pending_writer.dwpt.lock().has_flushed());

  let thread_pool_size = w.doc_writer.flush_control.per_thread_pool.size();

  {
    let guard = w.doc_writer.guard.lock();
    w.doc_writer
      .flush_control
      .mark_for_full_flush(&w.doc_writer, &guard, &w.config)?;
  }

  let documents_writer_per_thread = w
    .doc_writer
    .flush_control
    .checkout_largest_non_pending_writer(&w.config)?;

  assert!(documents_writer_per_thread.is_none());
  assert_eq!(
    thread_pool_size,
    w.doc_writer.flush_control.num_queued_flushes()
  );

  w.doc_writer
    .flush_control
    .abort_full_flushes(&w.doc_writer, &w.config)?;

  assert!(
    w.doc_writer
      .flush_control
      .checkout_largest_non_pending_writer(&w.config)?
      .is_none(),
    "was aborted"
  );

  assert_eq!(0, w.doc_writer.flush_control.num_queued_flushes());

  w.close()?;

  Ok(())
}

#[test]
fn test_apply_deletes_without_flushes() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut index_writer_config = IndexWriterConfig::new()?;
  let flush_deletes = Arc::new(AtomicBool::new(false));
  index_writer_config.set_flush_policy(ApplyDeletesFlushPolicy::new(flush_deletes.clone()));
  let w = IndexWriter::new(dir.clone(), index_writer_config)?;

  assert_eq!(0, w.doc_writer.flush_control.get_delete_bytes_used()?);
  w.delete_documents_with_terms(vec![Term::from_text("foo", "bar")])?;
  let mut bytes_used = w.doc_writer.flush_control.get_delete_bytes_used()?;
  assert!(bytes_used > 0, "{bytes_used} > 0");
  w.delete_documents_with_terms(vec![Term::from_text("foo", "baz")])?;
  bytes_used = w.doc_writer.flush_control.get_delete_bytes_used()?;
  assert!(bytes_used > 0, "{bytes_used} > 0");
  assert_eq!(2, w.doc_writer.get_buffered_delete_terms_size()?);
  assert_eq!(0, w.get_flush_deletes_count());
  flush_deletes.store(true, SeqCst);
  w.delete_documents_with_terms(vec![Term::from_text("foo", "bar")])?;
  assert_eq!(0, w.doc_writer.flush_control.get_delete_bytes_used()?);
  assert_eq!(1, w.get_flush_deletes_count());

  w.close()?;
  Ok(())
}
#[test]
fn test_deletes_applied_on_flush() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut field_types = HashMap::new();
  {
    let w = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;
    let mut doc = Document::new();
    doc.add(new_field(
      &mut random,
      "id",
      "1",
      &STORED_TEXT_TYPE,
      &mut field_types,
    )?);
    w.add_document(doc.clone())?;
    w.update_document_with_term(Term::from_text("id", "1"), doc)?;
    let mut delete_bytes_used = w.doc_writer.flush_control.get_delete_bytes_used()?;
    assert!(
      delete_bytes_used > 0,
      "deletedBytesUsed: {delete_bytes_used}"
    );
    assert_eq!(0, w.get_flush_deletes_count());
    assert!(w.flush_next_buffer()?);
    assert_eq!(1, w.get_flush_deletes_count());
    assert_eq!(0, w.doc_writer.flush_control.get_delete_bytes_used()?);
    w.delete_all()?;
    w.commit()?;
    assert_eq!(2, w.get_flush_deletes_count());
    if random.random_bool(0.5) {
      w.delete_documents_with_terms(vec![Term::from_text("id", "1")])?;
    } else {
      w.update_doc_values(
        Term::from_text("id", "1"),
        vec![NumericDocValuesField::new("foo", 1).into()],
      )?;
    }
    delete_bytes_used = w.doc_writer.flush_control.get_delete_bytes_used()?;
    assert!(
      delete_bytes_used > 0,
      "deletedBytesUsed: {delete_bytes_used}"
    );
    doc = Document::new();
    doc.add(new_field(
      &mut random,
      "id",
      "5",
      &STORED_TEXT_TYPE,
      &mut field_types,
    )?);
    w.add_document(doc)?;
    assert!(w.flush_next_buffer()?);
    assert_eq!(0, w.doc_writer.flush_control.get_delete_bytes_used()?);
    assert_eq!(3, w.get_flush_deletes_count());
    w.close()?;
  }

  {
    let w = RandomIndexWriter::with_config(&mut random, dir.clone(), IndexWriterConfig::new()?);
    let num_docs = random.random_range(1..100);
    for i in 0..num_docs {
      let mut doc = Document::new();
      doc.add(new_field(
        &mut random,
        "id",
        i.to_string(),
        &STORED_TEXT_TYPE,
        &mut field_types,
      )?);
      w.add_document(&mut random, doc)?;
    }
    for i in 0..num_docs {
      if random.random_bool(0.5) {
        let mut doc = Document::new();
        doc.add(new_field(
          &mut random,
          "id",
          i.to_string(),
          &STORED_TEXT_TYPE,
          &mut field_types,
        )?);
        w.update_document_with_term(&mut random, Term::from_text("id", i.to_string()), doc)?;
      }
    }

    let delete_bytes_used = w.w.doc_writer.flush_control.get_delete_bytes_used()?;
    if delete_bytes_used > 0 {
      assert!(w.w.flush_next_buffer()?);
      assert_eq!(0, w.w.doc_writer.flush_control.get_delete_bytes_used()?);
    }
    w.close(&mut random)?;
  }

  Ok(())
}

#[test]
fn test_hold_lock_on_largest_writer() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let w = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;
  let num_docs = index_docs_for_multiple_dwpts(&w, &mut random)?;

  let largest_non_pending_writer = w
    .doc_writer
    .flush_control
    .find_largest_non_pending_writer()?
    .unwrap();
  assert!(!largest_non_pending_writer.dwpt.lock().is_flush_pending());
  assert!(!largest_non_pending_writer.dwpt.lock().has_flushed());

  let locked = Arc::new(Barrier::new(3));
  let wait = Arc::new(Barrier::new(2));

  thread::scope(|scope| -> Result<()> {
    let lock_thread = {
      let largest_non_pending_writer = Arc::clone(&largest_non_pending_writer);
      let locked = Arc::clone(&locked);
      let wait = Arc::clone(&wait);
      scope.spawn(move || {
        largest_non_pending_writer.lock();
        locked.wait();
        wait.wait();
        largest_non_pending_writer.unlock();
      })
    };

    let flush_thread = {
      let locked = Arc::clone(&locked);
      let writer = &w;
      scope.spawn(move || -> Result<()> {
        locked.wait();
        assert!(writer.flush_next_buffer()?);
        Ok(())
      })
    };

    locked.wait();
    // Access a synced method to ensure we never lock while we hold the flush control monitor.
    w.doc_writer.flush_control.active_bytes(None);
    wait.wait();

    lock_thread.join().expect("thread panicked");
    flush_thread.join().expect("thread panicked")?;

    Ok(())
  })?;

  assert!(
    largest_non_pending_writer.dwpt.lock().has_flushed(),
    "largest DWPT should be flushed"
  );

  // Make sure it's not locked.
  largest_non_pending_writer.lock();
  largest_non_pending_writer.unlock();

  if random.random_bool(0.5) {
    w.commit()?;
  }

  let reader = directory_reader::open_with_writer_deletes(&w, true, true)?;
  assert_eq!(num_docs, reader.num_docs()?);

  w.close()?;

  Ok(())
}

#[test]
fn test_check_pending_flush_post_update() -> Result<()> {
  let mut random = random();
  let dir = Arc::new(new_mock_directory(&mut random)?);
  let flushing_threads = Arc::new(Mutex::new(HashSet::new()));
  dir.fail_on(Box::new(FlushFailure {
    flushing_threads: Arc::clone(&flushing_threads),
    do_fail: false,
  }));
  let mut config = IndexWriterConfig::new()?;
  config.set_check_pending_flush_update(false);
  config.set_max_buffered_docs(i32::MAX);
  config.set_ram_buffer_size_mb(DISABLE_AUTO_FLUSH as f64);
  let w = IndexWriter::new(dir.clone(), config)?;
  let done = AtomicBool::new(false);
  let num_threads = 2 + random.random_range(0..3);
  let latch = Barrier::new(num_threads + 1);
  let indexing_threads = Arc::new(Mutex::new(HashSet::new()));

  let body_result = thread::scope(|scope| -> Result<()> {
    let mut threads = Vec::new();
    for _ in 0..num_threads {
      let thread = scope.spawn(|| -> Result<()> {
        latch.wait();
        let mut num_docs = 0;
        while !done.load(SeqCst) {
          let mut doc = Document::new();
          doc.add(StringField::from_string("id", "foo", Store::Yes)?);
          w.add_document(doc)?;
          if num_docs % 10 == 0 {
            thread::yield_now();
          }
          num_docs += 1;
        }
        Ok(())
      });
      indexing_threads
        .lock()
        .unwrap()
        .insert(thread.thread().id());
      threads.push(thread);
    }
    latch.wait();

    let result = (|| -> Result<()> {
      let num_iters = if rarely(&mut random) {
        1 + random.random_range(0..5)
      } else {
        1
      };
      for _ in 0..num_iters {
        wait_for_docs_in_buffers(&w, std::cmp::min(2, num_threads));
        w.commit()?;
        let mut flushing_threads = flushing_threads.lock().unwrap();
        assert!(
          flushing_threads.contains(&thread::current().id()),
          "{flushing_threads:?}"
        );
        flushing_threads.retain(|thread| indexing_threads.lock().unwrap().contains(thread));
        assert!(flushing_threads.is_empty(), "{flushing_threads:?}");
      }
      w.get_config().set_check_pending_flush_update(true);
      let mut num_iters = 0;
      loop {
        assert!(num_iters < 100, "should finish in less than 100 iterations");
        num_iters += 1;
        wait_for_docs_in_buffers(&w, std::cmp::min(2, num_threads));
        w.flush()?;
        let mut flushing_threads = flushing_threads.lock().unwrap();
        flushing_threads.retain(|thread| indexing_threads.lock().unwrap().contains(thread));
        if !flushing_threads.is_empty() {
          break;
        }
      }
      Ok(())
    })();

    done.store(true, SeqCst);
    for handle in threads {
      handle.join().expect("thread panicked")?;
    }
    result
  });
  let writer_close_result = w.close();
  let close_result = IOUtils::use_or_suppress_result(writer_close_result, dir.as_ref().close());
  IOUtils::use_or_suppress_result(body_result, close_result)
}

struct FlushFailure {
  flushing_threads: Arc<Mutex<HashSet<thread::ThreadId>>>,
  do_fail: bool,
}

impl<D> Failure<D> for FlushFailure
where
  D: Directory,
{
  fn eval(&mut self, _dir: &MockDirectoryWrapper<D>) -> Result<()> {
    if call_stack_contains::<DocumentsWriterPerThread<D>>("flush") {
      self
        .flushing_threads
        .lock()
        .unwrap()
        .insert(thread::current().id());
    }
    Ok(())
  }

  fn set_do_fail(&mut self) {
    self.do_fail = true;
  }

  fn clear_do_fail(&mut self) {
    self.do_fail = false;
  }
}

fn wait_for_docs_in_buffers<D>(w: &IndexWriter<D>, buffers_with_docs: usize)
where
  D: Directory,
{
  // wait until at least N DWPTs have a doc in order to observe who flushes the segments.
  loop {
    let mut num_states_with_docs = 0;
    let per_thread_pool = &w.doc_writer.flush_control.per_thread_pool;
    for (_id, dwpt) in per_thread_pool.iterator() {
      dwpt.lock();
      let num_docs_in_ram = dwpt.dwpt.lock().get_num_docs_in_ram();
      dwpt.unlock();
      if num_docs_in_ram > 1 {
        num_states_with_docs += 1;
      }
    }
    if num_states_with_docs >= buffers_with_docs {
      return;
    }
    thread::yield_now();
  }
}

#[test]
fn test_soft_update_documents() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut config = new_index_writer_config(&mut random)?;
  config
    .set_merge_policy(NoMergePolicy::default())
    .set_soft_deletes_field("soft_delete");
  let writer = IndexWriter::new(dir.clone(), config)?;

  let err = writer.soft_update_document(
    Term::from_text("id", "1"),
    Document::new(),
    Vec::<crate::core::document::fields::Fields>::new(),
  );
  match err {
    Ok(_) => panic!("expected IllegalArgument error"),
    Err(err) => {
      assert!(matches!(err, LuceneError::IllegalArgument(_)));
      assert_eq!("at least one soft delete must be present", err.to_string());
    },
  }

  let err = writer.soft_update_documents(
    Term::from_text("id", "1"),
    vec![vec![
      StringField::from_string("id", "1", Store::Yes)?.into(),
    ]],
    Vec::<crate::core::document::fields::Fields>::new(),
  );
  match err {
    Ok(_) => panic!("expected IllegalArgument error"),
    Err(err) => {
      assert!(matches!(err, LuceneError::IllegalArgument(_)));
      assert_eq!("at least one soft delete must be present", err.to_string());
    },
  }

  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "1", Store::Yes)?);
  doc.add(StringField::from_string("version", "1", Store::Yes)?);
  writer.add_document(doc)?;

  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "1", Store::Yes)?);
  doc.add(StringField::from_string("version", "2", Store::Yes)?);
  writer.soft_update_document(
    Term::from_text("id", "1"),
    doc,
    vec![NumericDocValuesField::new("soft_delete", 1).into()],
  )?;

  let mut reader = directory_reader::open_from_writer(&writer)?;
  assert_eq!(2, reader.doc_freq(&Term::from_text("id", "1"))?);
  let searcher = new_searcher_with_reader(directory_reader::open_from_writer(&writer)?)?;
  let top_docs = searcher.search(TermQuery::new(Term::from_text("id", "1")), 10)?;
  assert_eq!(1, top_docs.score_docs.len());
  let mut stored_fields = searcher.stored_fields()?;
  let document = stored_fields.document(top_docs.score_docs[0].doc)?;
  let version = document
    .get_field("version")
    .expect("version field should exist")
    .string_value()?
    .expect("version should have a string value");
  assert_eq!("2", version.as_ref());

  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "1", Store::Yes)?);
  doc.add(StringField::from_string("version", "3", Store::Yes)?);
  writer.soft_update_document(
    Term::from_text("id", "1"),
    doc,
    vec![NumericDocValuesField::new("soft_delete", 1).into()],
  )?;

  let old_reader = reader;
  reader = directory_reader::open_if_changed(&old_reader)?.expect("reader should change");
  old_reader.close()?;
  let searcher = new_searcher_with_reader(directory_reader::open_from_writer(&writer)?)?;
  let top_docs = searcher.search(TermQuery::new(Term::from_text("id", "1")), 10)?;
  assert_eq!(1, top_docs.score_docs.len());
  let mut stored_fields = searcher.stored_fields()?;
  let document = stored_fields.document(top_docs.score_docs[0].doc)?;
  let version = document
    .get_field("version")
    .expect("version field should exist")
    .string_value()?
    .expect("version should have a string value");
  assert_eq!("3", version.as_ref());

  writer.update_doc_values(
    Term::from_text("id", "1"),
    vec![NumericDocValuesField::new("soft_delete", 1).into()],
  )?;

  let old_reader = reader;
  reader = directory_reader::open_if_changed(&old_reader)?.expect("reader should change");
  old_reader.close()?;
  let searcher = new_searcher_with_reader(directory_reader::open_from_writer(&writer)?)?;
  let top_docs = searcher.search(TermQuery::new(Term::from_text("id", "1")), 10)?;
  assert_eq!(0, top_docs.total_hits.value());

  let mut num_soft_deleted = 0;
  for info in writer.clone_segment_infos()?.iter() {
    num_soft_deleted += info.get_soft_del_count();
  }
  let doc_stats = writer.get_doc_stats()?;
  assert_eq!(doc_stats.max_doc - doc_stats.num_docs, num_soft_deleted);

  for leaf in reader.get_context()?.leaves()? {
    assert!(leaf.reader().get_hard_live_docs()?.is_none());
  }

  writer.close()?;
  Ok(())
}

#[test]
fn test_soft_updates_concurrently() -> Result<()> {
  soft_updates_concurrently(false)
}

#[test]
fn test_soft_updates_concurrently_mixed_deletes() -> Result<()> {
  soft_updates_concurrently(true)
}

fn soft_updates_concurrently(mix_deletes: bool) -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut index_writer_config = new_index_writer_config(&mut random)?;
  index_writer_config.set_soft_deletes_field("soft_delete");
  let merge_away_soft_deletes = Arc::new(AtomicBool::new(random.random_bool(0.5)));
  if !mix_deletes {
    let merge_policy = index_writer_config.get_merge_policy().clone();
    index_writer_config.set_merge_policy(SoftUpdatesConcurrentlyMergePolicy::new(
      OneMergeWrappingMergePolicy::new(
        merge_policy,
        SoftUpdatesConcurrentlyOneMergeUnaryOperator::new(merge_away_soft_deletes.clone()),
      ),
      merge_away_soft_deletes.clone(),
    ));
  }
  let writer = IndexWriter::new(dir.clone(), index_writer_config)?;

  let body_result = (|| -> Result<()> {
    let num_threads = 2 + random.random_range(0..3);
    let start_latch = Arc::new(CountDownLatch::new(1));
    let started = Arc::new(CountDownLatch::new(num_threads));
    let update_several_docs = random.random_bool(0.5);
    let ids = Arc::new(Mutex::new(HashSet::new()));
    let seeds: Vec<u64> = (0..num_threads).map(|_| random.random()).collect();

    thread::scope(|scope| -> Result<()> {
      let mut threads = Vec::with_capacity(num_threads);
      for seed in seeds {
        let writer = writer.clone();
        let start_latch = start_latch.clone();
        let started = started.clone();
        let ids = ids.clone();
        threads.push(scope.spawn(move || -> Result<()> {
          let mut random = random_from_seed(seed);
          started.count_down();
          start_latch.wait();
          for _ in 0..100 {
            let id = random.random_range(0..10).to_string();
            let mut doc = Document::new();
            doc.add(StringField::from_string("id", &id, Store::Yes)?);
            if update_several_docs {
              let docs = vec![
                doc
                  .clone()
                  .into_iter()
                  .collect::<Vec<crate::core::document::fields::Fields>>(),
                doc
                  .into_iter()
                  .collect::<Vec<crate::core::document::fields::Fields>>(),
              ];
              if mix_deletes && random.random_bool(0.5) {
                if random.random_bool(0.5) {
                  writer.update_documents_with_term(Term::from_text("id", &id), docs)?;
                } else {
                  writer.update_documents_with_query(
                    Some(TermQuery::new(Term::from_text("id", &id)).into()),
                    docs,
                  )?;
                }
              } else {
                writer.soft_update_documents(
                  Term::from_text("id", &id),
                  docs,
                  vec![NumericDocValuesField::new("soft_delete", 1).into()],
                )?;
              }
            } else if mix_deletes && random.random_bool(0.5) {
              writer.update_document_with_term(Term::from_text("id", &id), doc)?;
            } else {
              writer.soft_update_document(
                Term::from_text("id", &id),
                doc,
                vec![NumericDocValuesField::new("soft_delete", 1).into()],
              )?;
            }
            ids.lock().expect("ids mutex poisoned").insert(id);
          }
          Ok(())
        }));
      }
      started.wait();
      start_latch.count_down();
      for thread in threads {
        match thread.join() {
          Ok(result) => result?,
          Err(payload) => {
            return Err(LuceneError::tragedy_from_panic(
              "panic while applying soft updates",
              payload.as_ref(),
            ));
          },
        }
      }
      Ok(())
    })?;

    let mut reader = Arc::new(directory_reader::open_from_writer(&writer)?);
    let reader_result = (|| -> Result<()> {
      let ids = ids.lock().expect("ids mutex poisoned").clone();
      let searcher = new_searcher_with_reader(reader.clone())?;
      for id in &ids {
        let top_docs = searcher.search(TermQuery::new(Term::from_text("id", id)), 10)?;
        if update_several_docs {
          assert_eq!(2, top_docs.total_hits.value());
          assert_eq!(
            1,
            (top_docs.score_docs[0].doc - top_docs.score_docs[1].doc).abs()
          );
        } else {
          assert_eq!(1, top_docs.total_hits.value());
        }
      }
      drop(searcher);
      if !mix_deletes {
        for context in reader.clone().get_context()?.leaves()? {
          assert!(context.reader().get_hard_live_docs()?.is_none());
        }
      }

      merge_away_soft_deletes.store(true, SeqCst);
      writer.add_document(Document::new())?; // Add a dummy doc to trigger a segment here.
      writer.flush()?;
      writer.force_merge(1)?;
      if let Some(new_reader) =
        directory_reader::open_if_changed_with_writer(reader.as_ref(), &writer)?
      {
        reader.close()?;
        reader = Arc::new(new_reader);
      }
      for id in &ids {
        if update_several_docs {
          assert_eq!(2, reader.doc_freq(&Term::from_text("id", id))?);
        } else {
          assert_eq!(1, reader.doc_freq(&Term::from_text("id", id))?);
        }
      }
      let mut num_soft_deleted = 0;
      for info in writer.clone_segment_infos()?.iter() {
        num_soft_deleted += info.get_soft_del_count() + info.get_del_count();
      }
      let doc_stats = writer.get_doc_stats()?;
      assert_eq!(doc_stats.max_doc - doc_stats.num_docs, num_soft_deleted);
      writer.commit()?;

      let dir_reader = Arc::new(directory_reader::open(dir.clone())?);
      let dir_reader_result = (|| -> Result<()> {
        let mut del_count = 0;
        for context in dir_reader.clone().get_context()?.leaves()? {
          let segment_info = context.reader().get_segment_info();
          del_count += segment_info.get_soft_del_count() + segment_info.get_del_count();
        }
        assert_eq!(num_soft_deleted, del_count);
        Ok(())
      })();
      IOUtils::use_or_suppress_result(dir_reader_result, dir_reader.close())
    })();
    IOUtils::use_or_suppress_result(reader_result, reader.close())
  })();

  let close_result = IOUtils::use_or_suppress_result(writer.close(), dir.close());
  IOUtils::use_or_suppress_result(body_result, close_result)
}

#[test]
fn test_delete_happens_before_while_flush() -> Result<()> {
  let mut random = random();
  let latch = Arc::new(CountDownLatch::new(1));
  let in_flush = Arc::new(CountDownLatch::new(1));
  let dir = Arc::new(BlockOnIndexingChainFlushDirectory::new(
    new_directory(&mut random)?,
    latch.clone(),
    in_flush.clone(),
  ));
  let writer = IndexWriter::new(dir.clone(), new_index_writer_config(&mut random)?)?;

  let mut document = Document::new();
  document.add(StringField::from_string("id", "1", Store::Yes)?);
  writer.add_document(document.clone())?;
  let update_document = random.random_bool(0.5);
  thread::scope(|scope| -> Result<()> {
    let update_thread = scope.spawn(|| {
      in_flush.wait();
      writer.doc_writer.flush_control.set_apply_all_deletes();
      let result = if update_document {
        writer
          .update_document_with_term(Term::from_text("id", "1"), document)
          .map(|_| ())
      } else {
        writer
          .delete_documents_with_terms(vec![Term::from_text("id", "1")])
          .map(|_| ())
      };
      latch.count_down();
      result
    });

    let reader = directory_reader::open_from_writer(&writer)?;
    assert_eq!(1, reader.num_docs()?);
    reader.close()?;
    update_thread.join().expect("update thread panicked")?;
    Ok(())
  })?;

  writer.close()?;
  dir.in_.close()?;
  Ok(())
}

struct BlockOnIndexingChainFlushDirectory<D> {
  in_: D,
  latch: Arc<CountDownLatch>,
  in_flush: Arc<CountDownLatch>,
  id: Identity,
}

impl<D> BlockOnIndexingChainFlushDirectory<D>
where
  D: Directory,
{
  fn new(in_: D, latch: Arc<CountDownLatch>, in_flush: Arc<CountDownLatch>) -> Self {
    Self {
      in_,
      latch,
      in_flush,
      id: Identity::new(),
    }
  }
}

impl<D> Display for BlockOnIndexingChainFlushDirectory<D>
where
  D: Directory,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "BlockOnIndexingChainFlushDirectory({})", self.in_)
  }
}

impl<D> CloseableRef for BlockOnIndexingChainFlushDirectory<D>
where
  D: Directory,
{
  fn close(&self) -> Result<()> {
    self.in_.close()
  }
}

impl<D> HasIdentity for BlockOnIndexingChainFlushDirectory<D>
where
  D: Directory,
{
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl<D> Directory for BlockOnIndexingChainFlushDirectory<D>
where
  D: Directory,
{
  fn list_all(&self) -> Result<Vec<String>> {
    self.in_.list_all()
  }

  fn delete_file(&self, name: &str) -> Result<()> {
    self.in_.delete_file(name)
  }

  fn file_length(&self, name: &str) -> Result<usize> {
    self.in_.file_length(name)
  }

  type IndexOutput = D::IndexOutput;

  fn create_output(&self, name: &str, context: &IOContext) -> Result<Self::IndexOutput> {
    if call_stack_contains::<IndexingChain<D>>("flush") {
      self.in_flush.count_down();
      self.latch.wait();
    }
    self.in_.create_output(name, context)
  }

  fn create_temp_output(
    &self,
    prefix: &str,
    suffix: &str,
    context: &IOContext,
  ) -> Result<Self::IndexOutput> {
    self.in_.create_temp_output(prefix, suffix, context)
  }

  fn sync(&self, names: &[String]) -> Result<()> {
    self.in_.sync(names)
  }

  fn sync_metadata(&self) -> Result<()> {
    self.in_.sync_metadata()
  }

  fn rename(&self, source: &str, dest: &str) -> Result<()> {
    self.in_.rename(source, dest)
  }

  type IndexInput = D::IndexInput;

  fn open_input(&self, name: &str, context: &IOContext) -> Result<Self::IndexInput> {
    self.in_.open_input(name, context)
  }

  type Lock = D::Lock;

  fn obtain_lock(&self, name: &str) -> Result<Self::Lock> {
    self.in_.obtain_lock(name)
  }

  fn get_pending_deletions(&self) -> Result<HashSet<String>> {
    self.in_.get_pending_deletions()
  }

  #[cfg(debug_assertions)]
  fn is_fs_directory(&self) -> bool {
    self.in_.is_fs_directory()
  }

  fn ensure_open(&self) -> Result<()> {
    self.in_.ensure_open()
  }
}

fn assert_files<D>(writer: &IndexWriter<D>) -> Result<()>
where
  D: Directory,
{
  use std::collections::HashSet;

  let filter = |file: &str| !file.starts_with("segments") && file != "write.lock";
  // remove segment files we don't know if we have committed and what is kept around
  let seg_files: HashSet<String> = writer
    .clone_segment_infos()?
    .files(true)?
    .into_iter()
    .filter(|f| filter(f))
    .collect();

  let dir_files: HashSet<String> = writer
    .get_directory()
    .list_all()?
    .into_iter()
    .filter(|f| f != EXTRA_FILE_NAME)
    .filter(|f| filter(f))
    .collect();

  assert_eq!(seg_files.len(), dir_files.len(),);

  Ok(())
}

#[test]
fn test_fully_deleted_segments_release_files() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let mut config = new_index_writer_config(&mut random)?;
  config.set_ram_buffer_size_mb(i32::MAX as f64);
  config.set_max_buffered_docs(2);
  let writer = IndexWriter::new(dir.clone(), config)?;

  let mut d = Document::new();
  d.add(StringField::from_string("id", "doc-0", Store::Yes)?);
  writer.add_document(d)?;
  writer.flush()?;

  let mut d = Document::new();
  d.add(StringField::from_string("id", "doc-1", Store::Yes)?);
  writer.add_document(d)?;
  writer.delete_documents_with_terms(vec![Term::from_text("id", "doc-1")])?;

  assert_eq!(1, writer.clone_segment_infos()?.size());
  writer.flush()?;
  assert_eq!(1, writer.clone_segment_infos()?.size());
  writer.commit()?;

  assert_files(&writer)?;
  assert_eq!(1, writer.clone_segment_infos()?.size());
  writer.close()?;
  Ok(())
}

#[test]
fn test_segment_info_is_snapshot() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let mut config = new_index_writer_config(&mut random)?;
  config.set_ram_buffer_size_mb(i32::MAX as f64);
  config.set_max_buffered_docs(2);
  let writer = IndexWriter::new(dir.clone(), config)?;

  let mut d = Document::new();
  d.add(StringField::from_string("id", "doc-0", Store::Yes)?);
  writer.add_document(d)?;

  let mut d = Document::new();
  d.add(StringField::from_string("id", "doc-1", Store::Yes)?);
  writer.add_document(d)?;

  let reader = directory_reader::open_from_writer(&writer)?;
  let context = reader.get_context()?;
  let r = context.leaves()?;
  let segment_reader = r.first().unwrap().reader();
  let segment_info = segment_reader.get_segment_info();
  let original_info_id = segment_reader.get_original_segment_info_id();
  let clone_segment_infos = writer.clone_segment_infos()?;
  let original_info = clone_segment_infos.index_of(original_info_id).unwrap();

  assert_eq!(0, original_info.get_del_count());
  assert_eq!(0, segment_info.get_del_count());

  writer.delete_documents_with_terms(vec![Term::from_text("id", "doc-0")])?;
  writer.commit()?;
  // snapshot
  assert_eq!(0, segment_info.get_del_count());
  writer.close()?;
  Ok(())
}

#[test]
fn test_prevent_changing_soft_deletes_field() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut config = new_index_writer_config(&mut random)?;
  config.set_soft_deletes_field("my_deletes");
  let writer = IndexWriter::new(dir.clone(), config)?;
  let mut v1 = Document::new();
  v1.add(StringField::from_string("id", "1", Store::Yes)?);
  v1.add(StringField::from_string("version", "1", Store::Yes)?);
  writer.add_document(v1)?;
  let mut v2 = Document::new();
  v2.add(StringField::from_string("id", "1", Store::Yes)?);
  v2.add(StringField::from_string("version", "2", Store::Yes)?);
  writer.soft_update_document(
    Term::from_text("id", "1"),
    v2,
    vec![NumericDocValuesField::new("my_deletes", 1).into()],
  )?;
  writer.commit()?;
  writer.close()?;
  drop(writer);

  for si in SegmentInfos::read_latest_commit(dir.clone())?.iter() {
    let field_infos = read_field_infos(si)?;
    assert_eq!(
      Some("my_deletes"),
      field_infos.get_soft_deletes_field().map(String::as_str)
    );
    assert!(
      field_infos
        .field_info_by_name("my_deletes")?
        .expect("soft-deletes field should exist")
        .is_soft_deletes_field()
    );
  }

  let mut config = new_index_writer_config(&mut random)?;
  config.set_soft_deletes_field("your_deletes");
  let illegal_error = IndexWriter::new(dir.clone(), config)
    .err()
    .expect("changing the soft-deletes field should fail");
  assert!(matches!(illegal_error, LuceneError::IllegalArgument(_)));
  assert_eq!(
    "cannot configure [your_deletes] as soft-deletes; this index uses [my_deletes] as soft-deletes already",
    illegal_error.to_string()
  );

  let mut soft_delete_config = new_index_writer_config(&mut random)?;
  soft_delete_config.set_soft_deletes_field("my_deletes");
  soft_delete_config.set_merge_policy(SoftDeletesRetentionMergePolicy::new(
    "my_deletes",
    || Ok(MatchAllDocsQuery::new().into()),
    new_merge_policy(&mut random)?,
  ));
  let writer = IndexWriter::new(dir.clone(), soft_delete_config)?;
  let mut tombstone = Document::new();
  tombstone.add(StringField::from_string("id", "tombstone", Store::Yes)?);
  tombstone.add(NumericDocValuesField::new("my_deletes", 1));
  writer.add_document(tombstone)?;
  writer.flush()?;
  for si in writer.clone_segment_infos()?.iter() {
    let field_infos = read_field_infos(si)?;
    assert_eq!(
      Some("my_deletes"),
      field_infos.get_soft_deletes_field().map(String::as_str)
    );
    assert!(
      field_infos
        .field_info_by_name("my_deletes")?
        .expect("soft-deletes field should exist")
        .is_soft_deletes_field()
    );
  }
  writer.close()?;
  drop(writer);

  // Reopening a writer without a soft-deletes field should be prevented.
  let config = new_index_writer_config(&mut random)?;
  let reopen_error = IndexWriter::new(dir.clone(), config)
    .err()
    .expect("omitting the configured soft-deletes field should fail");
  assert!(matches!(reopen_error, LuceneError::IllegalArgument(_)));
  assert_eq!(
    "this index has [my_deletes] as soft-deletes already but soft-deletes field is not configured in IWC",
    reopen_error.to_string()
  );
  dir.close()
}
#[test]
fn test_prevent_adding_indexes_with_different_soft_deletes_field() -> Result<()> {
  let mut random = random();

  let dir1 = new_directory_shared(&mut random)?;
  let mut config = new_index_writer_config(&mut random)?;
  config.set_soft_deletes_field("soft_deletes_1");
  let w1 = IndexWriter::new(dir1.clone(), config)?;

  for i in 0..2 {
    let mut d = Document::new();
    d.add(StringField::from_string("id", "1", Store::Yes)?);
    d.add(StringField::from_string(
      "version",
      i.to_string(),
      Store::Yes,
    )?);

    w1.soft_update_document(
      Term::from_text("id", "1"),
      d,
      vec![NumericDocValuesField::new("soft_deletes_1", 1).into()],
    )?;
  }

  w1.commit()?;
  w1.close()?;
  drop(w1);

  let dir2 = new_directory_shared(&mut random)?;
  let mut config = new_index_writer_config(&mut random)?;
  config.set_soft_deletes_field("soft_deletes_2");
  let w2 = IndexWriter::new(dir2.clone(), config)?;

  let err = w2.add_indexes_from_directory(std::slice::from_ref(&dir1));
  match err {
    Ok(_) => panic!("expected IllegalArgument error"),
    Err(err) => {
      assert!(matches!(err, LuceneError::IllegalArgument(_)));
      assert_eq!(
        "cannot configure [soft_deletes_2] as soft-deletes; this index uses [soft_deletes_1] as soft-deletes already",
        err.to_string()
      );
    },
  }

  w2.close()?;

  let dir3 = new_directory_shared(&mut random)?;
  let mut config = new_index_writer_config(&mut random)?;
  config.set_soft_deletes_field("soft_deletes_1");
  let w3 = IndexWriter::new(dir3, config)?;

  w3.add_indexes_from_directory(std::slice::from_ref(&dir1))?;

  for si in w3.clone_segment_infos()?.iter() {
    let field_infos = read_field_infos(si)?;
    let soft_delete_field = field_infos.field_info_by_name("soft_deletes_1")?.unwrap();
    assert!(soft_delete_field.is_soft_deletes_field());
  }

  w3.close()?;

  Ok(())
}

#[test]
fn test_not_allow_using_existing_field_as_soft_deletes() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let w = IndexWriter::new(dir.clone(), new_index_writer_config(&mut random)?)?;

  for _ in 0..2 {
    let mut d = Document::new();
    d.add(StringField::from_string("id", "1", Store::Yes)?);

    if random.random_bool(0.5) {
      d.add(NumericDocValuesField::new("dv_field", 1));
      w.update_document_with_term(Term::from_text("id", "1"), d)?;
    } else {
      w.soft_update_document(
        Term::from_text("id", "1"),
        d,
        vec![NumericDocValuesField::new("dv_field", 1).into()],
      )?;
    }
  }

  w.commit()?;
  w.close()?;
  drop(w);
  let soft_deletes_field = if random.random_bool(0.5) {
    "id"
  } else {
    "dv_field"
  };

  let mut config = new_index_writer_config(&mut random)?;
  config.set_soft_deletes_field(soft_deletes_field);

  let err = IndexWriter::new(dir.clone(), config);
  match err {
    Ok(_) => panic!("expected IllegalArgument error"),
    Err(err) => {
      assert!(matches!(err, LuceneError::IllegalArgument(_)));
      assert_eq!(
        format!(
          "cannot configure [{}] as soft-deletes; this index uses [{}] as non-soft-deletes already",
          soft_deletes_field, soft_deletes_field
        ),
        err.to_string()
      );
    },
  }

  let mut config = new_index_writer_config(&mut random)?;
  config.set_soft_deletes_field("non-existing-field");

  let w = IndexWriter::new(dir, config)?;
  w.close()?;

  Ok(())
}

#[test]
fn test_broken_payload() -> Result<()> {
  let mut random = random();
  let d = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  let w = IndexWriter::new(d, iwc)?;

  let mut doc = Document::new();
  let mut token = token::with_range(Some("bar"), 0, 3)?;

  let mut evil = BytesRef::from_bytes(vec![0u8; 1024]);
  evil.offset = 1000;

  token.sub.token.set_payload(Some(evil));

  doc.add(TextField::from_token_stream(
    "foo",
    FieldTokenStreamEnum::custom(CannedTokenStream::new(vec![token])),
  )?);

  let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| w.add_document(doc)));
  assert!(result.is_err());
  Ok(())
}
#[test]
fn test_soft_and_hard_live_docs() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mut index_writer_config = new_index_writer_config(&mut random)?;
  let soft_deletes_field = "soft_delete";
  index_writer_config.set_soft_deletes_field(soft_deletes_field);

  let writer = IndexWriter::new(dir.clone(), index_writer_config)?;
  let mut unique_docs = HashSet::new();

  for _ in 0..100 {
    let doc_id = random.random_range(0..5);
    unique_docs.insert(doc_id);

    let mut doc = Document::new();
    doc.add(StringField::from_string(
      "id",
      doc_id.to_string(),
      Store::Yes,
    )?);

    if doc_id % 2 == 0 {
      writer.update_document_with_term(Term::from_text("id", doc_id.to_string()), doc)?;
    } else {
      writer.soft_update_document(
        Term::from_text("id", doc_id.to_string()),
        doc,
        vec![NumericDocValuesField::new(soft_deletes_field, 0).into()],
      )?;
    }

    if random.random_bool(0.5) {
      assert_hard_live_docs(&writer, &unique_docs)?;
    }
  }

  if random.random_bool(0.5) {
    writer.commit()?;
  }
  assert_hard_live_docs(&writer, &unique_docs)?;

  writer.close()?;

  Ok(())
}

#[test]
fn test_abort_fully_deleted_segment() -> Result<()> {
  let abort_merge_before_commit = Arc::new(AtomicBool::new(false));
  let mut random = random();
  let merge_policy = OneMergeWrappingMergePolicy::new(
    new_merge_policy(&mut random)?,
    AbortOnMergeCompleteOneMergeUnaryOperator::new(abort_merge_before_commit.clone()),
  );
  let merge_policy = KeepFullyDeletedSegmentsMergePolicy::new(merge_policy);

  let dir = new_directory_shared(&mut random)?;
  let mut index_writer_config = new_index_writer_config(&mut random)?;
  index_writer_config
    .set_merge_policy(merge_policy)
    .set_commit_on_close(false);
  let writer = IndexWriter::new(dir, index_writer_config)?;
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "1", Store::Yes)?);
  writer.add_document(doc)?;
  writer.flush()?;

  writer.delete_documents_with_terms(vec![Term::from_text("id", "1")])?;
  abort_merge_before_commit.store(true, SeqCst);
  writer.flush()?;
  writer.force_merge(1)?;
  writer.close()?;
  Ok(())
}

#[test]
fn test_set_index_created_version() -> Result<()> {
  let mut random = random();

  let mut iwc = IndexWriterConfig::<DirEnum>::new()?;
  let err = iwc.set_index_created_version_major(LATEST.major + 1);
  match err {
    Ok(_) => unreachable!("expected IllegalArgument error"),
    Err(err) => {
      assert!(matches!(err, LuceneError::IllegalArgument(_)));
      assert_eq!(
        format!(
          "indexCreatedVersionMajor may not be in the future: current major version is {}, but got: {}",
          LATEST.major,
          LATEST.major + 1
        ),
        err.to_string()
      );
    },
  }

  let mut iwc = IndexWriterConfig::<DirEnum>::new()?;
  let err = iwc.set_index_created_version_major(LATEST.major - 2);
  match err {
    Ok(_) => unreachable!("expected IllegalArgument error"),
    Err(err) => {
      assert!(matches!(err, LuceneError::IllegalArgument(_)));
      assert_eq!(
        format!(
          "indexCreatedVersionMajor may not be less than the minimum supported version: {}, but got: {}",
          LATEST.major - 1,
          LATEST.major - 2
        ),
        err.to_string()
      );
    },
  }

  for previous_major in LATEST.major - 1..=LATEST.major {
    for new_major in LATEST.major - 1..=LATEST.major {
      for open_mode in [OpenMode::Create, OpenMode::Append, OpenMode::CreateOrAppend] {
        let dir = new_directory_shared(&mut random)?;

        {
          let mut iwc = new_index_writer_config(&mut random)?;
          iwc.set_index_created_version_major(previous_major)?;
          let w = IndexWriter::new(dir.clone(), iwc)?;
          w.close()?;
        }

        let mut infos = SegmentInfos::read_latest_commit(dir.clone())?;
        assert_eq!(previous_major, infos.get_index_created_version_major());

        {
          let mut iwc = new_index_writer_config(&mut random)?;
          iwc.set_open_mode(open_mode);
          iwc.set_index_created_version_major(new_major)?;
          let w = IndexWriter::new(dir.clone(), iwc)?;
          w.close()?;
        }

        infos = SegmentInfos::read_latest_commit(dir)?;
        if open_mode == OpenMode::Create {
          assert_eq!(new_major, infos.get_index_created_version_major());
        } else {
          assert_eq!(previous_major, infos.get_index_created_version_major());
        }
      }
    }
  }

  Ok(())
}

#[test]
fn test_flush_while_starting_new_threads() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let w = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;
  w.add_document(Document::new())?;
  assert_eq!(1, w.doc_writer.flush_control.per_thread_pool.size());

  let latch = Barrier::new(2);

  thread::scope(|scope| -> Result<()> {
    let thread = scope.spawn(|| -> Result<()> {
      latch.wait();
      let mut states = Vec::new();
      let result = (|| -> Result<()> {
        for _ in 0..100 {
          let state = w
            .doc_writer
            .flush_control
            .per_thread_pool
            .get_and_lock(&w, || {
              w.doc_writer.flush_control.delete_queue.lock().clone()
            })?;
          state.state.delete_queue.get_next_sequence_number();
          states.push(state);
        }
        Ok(())
      })();
      for state in states {
        state.unlock();
      }
      result
    });

    latch.wait();
    {
      let guard = w.doc_writer.guard.lock();
      w.doc_writer
        .flush_control
        .mark_for_full_flush(&w.doc_writer, &guard, &w.config)?;
    }
    thread.join().expect("thread panicked")?;
    w.doc_writer
      .flush_control
      .abort_full_flushes(&w.doc_writer, &w.config)?;

    Ok(())
  })?;

  w.close()?;

  Ok(())
}

#[test]
fn test_refresh_and_rollback_concurrently() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let w = IndexWriter::new(dir.clone(), new_index_writer_config(&mut random)?)?;
  let stopped = AtomicBool::new(false);
  let indexed_docs = Semaphore::new(0);
  let indexer_seed = random.random();

  let sm = SearcherManager::from_writer(&w, Some(SearcherFactory::new()))?;

  let body_result = thread::scope(|scope| -> Result<()> {
    let indexer = scope.spawn(|| -> Result<()> {
      let mut random = random_from_seed(indexer_seed);
      while !stopped.load(SeqCst) {
        let id = random.random_range(0..100).to_string();
        let mut doc = Document::new();
        doc.add(StringField::from_string("id", id.clone(), Store::Yes)?);
        match w.update_document_with_term(Term::from_text("id", id), doc) {
          Ok(_) => indexed_docs.release(1),
          Err(LuceneError::AlreadyClosed(_)) => return Ok(()),
          Err(error) => return Err(error),
        }
      }
      Ok(())
    });

    let refresher = scope.spawn(|| -> Result<()> {
      while !stopped.load(SeqCst) {
        match sm.maybe_refresh_blocking() {
          Ok(()) => {},
          Err(LuceneError::AlreadyClosed(_)) => return Ok(()),
          Err(error) => return Err(error),
        }
      }
      Ok(())
    });

    indexed_docs.acquire(1 + random.random_range(0..100));
    let rollback_result = w.rollback();
    stopped.store(true, SeqCst);
    let indexer_result = indexer
      .join()
      .map_err(|_| LuceneError::illegal_state("indexer thread panicked"))?;
    let refresher_result = refresher
      .join()
      .map_err(|_| LuceneError::illegal_state("refresher thread panicked"))?;
    rollback_result?;
    indexer_result?;
    refresher_result?;

    assert!(
      w.get_tragic_exception().get().is_none(),
      "should not consider ACE a tragedy on a closed IW: {:?}",
      w.get_tragic_exception().get()
    );
    Ok(())
  });

  let close_result = IOUtils::close_refs_tuple((Some(&sm), Some(dir.as_ref())));
  IOUtils::use_or_suppress_result(body_result, close_result)
}

#[test]
fn test_closeable_queue() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let writer = IndexWriter::new(dir.clone(), new_index_writer_config(&mut random)?)?;
  let queue = Arc::new(EventQueue::new());
  let executed = Arc::new(AtomicI32::new(0));

  queue.add(EventEnum::Test(EventImplTest::new(executed.clone())))?;
  queue.add(EventEnum::Test(EventImplTest::new(executed.clone())))?;
  queue.process_events(&writer)?;
  assert_eq!(2, executed.load(SeqCst));
  queue.process_events(&writer)?;
  assert_eq!(2, executed.load(SeqCst));

  queue.add(EventEnum::Test(EventImplTest::new(executed.clone())))?;
  queue.add(EventEnum::Test(EventImplTest::new(executed.clone())))?;

  thread::scope(|scope| -> Result<()> {
    let thread_queue = queue.clone();
    let writer = &writer;
    let t = scope.spawn(move || -> Result<()> {
      match thread_queue.process_events(writer) {
        Ok(_) => Ok(()),
        Err(LuceneError::AlreadyClosed(_)) => Ok(()),
        Err(e) => Err(e),
      }
    });
    queue.close(writer)?;
    t.join().expect("thread panicked")?;
    Ok(())
  })?;

  assert_eq!(4, executed.load(SeqCst));
  let err = queue.process_events(&writer);
  assert!(matches!(err, Err(LuceneError::AlreadyClosed(_))));
  let err = queue.add(EventEnum::Test(EventImplTest::new(executed.clone())));
  assert!(matches!(err, Err(LuceneError::AlreadyClosed(_))));

  writer.close()?;
  Ok(())
}

#[test]
fn test_random_operations() -> Result<()> {
  let mut random = random();
  let mut iwc = new_index_writer_config(&mut random)?;
  let keep_fully_deleted_segment = Arc::new(AtomicBool::new(random.random_bool(0.5)));
  let merge_policy = new_merge_policy(&mut random)?;
  iwc.set_merge_policy(
    KeepFullyDeletedSegmentsMergePolicy::with_keep_fully_deleted_segments_and_merge_policy(
      merge_policy,
      keep_fully_deleted_segment,
    ),
  );
  let dir = new_directory_shared(&mut random)?;
  let writer = IndexWriter::new(dir.clone(), iwc)?;
  let sm = SearcherManager::from_writer(&writer, Some(SearcherFactory::new()))?;

  let num_operations = Semaphore::new(10 + random.random_range(0..1000));
  let single_doc = random.random_bool(0.5);
  let num_threads = 1 + random.random_range(0..4);
  let latch = CountDownLatch::new(num_threads);
  let seeds: Vec<u64> = (0..num_threads).map(|_| random.random()).collect();

  let body_result = thread::scope(|scope| -> Result<()> {
    let mut threads = Vec::with_capacity(num_threads);
    for seed in seeds {
      let latch = &latch;
      let num_operations = &num_operations;
      let writer = &writer;
      let sm = &sm;
      threads.push(scope.spawn(move || -> Result<()> {
        let mut random = random_from_seed(seed);
        latch.count_down();
        latch.wait();
        while num_operations.try_acquire() {
          let id = if single_doc {
            "1".to_string()
          } else {
            random.random_range(0..10).to_string()
          };
          let mut doc = Document::new();
          doc.add(StringField::from_string("id", id.clone(), Store::Yes)?);
          if random.random_range(0..10) <= 2 {
            writer.update_document_with_term(Term::from_text("id", id), doc)?;
          } else if random.random_range(0..10) <= 2 {
            writer.delete_documents_with_terms(vec![Term::from_text("id", id)])?;
          } else {
            writer.add_document(doc)?;
          }
          if random.random_range(0..100) < 10 {
            sm.maybe_refresh_blocking()?;
          }
          if random.random_range(0..100) < 5 {
            writer.commit()?;
          }
          if random.random_range(0..100) < 1 {
            writer
              .force_merge_with_wait(1 + random.random_range(0..10), random.random_bool(0.5))?;
          }
        }
        Ok(())
      }));
    }
    for thread in threads {
      thread
        .join()
        .map_err(|_| LuceneError::illegal_state("indexing thread panicked"))??;
    }
    Ok(())
  });

  let close_result = IOUtils::use_or_suppress_result(sm.close(), writer.close());
  let close_result = IOUtils::use_or_suppress_result(close_result, dir.close());
  IOUtils::use_or_suppress_result(body_result, close_result)
}

#[test]
fn test_random_operations_with_soft_deletes() -> Result<()> {
  let mut random = random();
  let mut iwc = new_index_writer_config(&mut random)?;
  let seq_no = Arc::new(AtomicI32::new(-1));
  let retaining_seq_no = Arc::new(AtomicI32::new(0));
  iwc.set_soft_deletes_field("soft_deletes");
  let merge_policy = new_merge_policy(&mut random)?;
  let retention_query_seq_no = retaining_seq_no.clone();
  iwc.set_merge_policy(SoftDeletesRetentionMergePolicy::new(
    "soft_deletes",
    move || {
      Ok(
        LongPoint::new_range_query(
          "seq_no",
          i64::from(retention_query_seq_no.load(SeqCst)),
          i64::MAX,
        )?
        .into(),
      )
    },
    merge_policy,
  ));
  let dir = new_directory_shared(&mut random)?;
  let writer = IndexWriter::new(dir.clone(), iwc)?;
  let sm = SearcherManager::from_writer(&writer, Some(SearcherFactory::new()))?;

  let num_operations = Semaphore::new(10 + random.random_range(0..1000));
  let single_doc = random.random_bool(0.5);
  let num_threads = 1 + random.random_range(0..4);
  let latch = CountDownLatch::new(num_threads);
  let seeds: Vec<u64> = (0..num_threads).map(|_| random.random()).collect();

  let body_result = thread::scope(|scope| -> Result<()> {
    let mut threads = Vec::with_capacity(num_threads);
    for seed in seeds {
      let latch = &latch;
      let num_operations = &num_operations;
      let writer = &writer;
      let sm = &sm;
      let seq_no = &seq_no;
      let retaining_seq_no = &retaining_seq_no;
      threads.push(scope.spawn(move || -> Result<()> {
        let mut random = random_from_seed(seed);
        latch.count_down();
        latch.wait();
        while num_operations.try_acquire() {
          let id = if single_doc {
            "1".to_string()
          } else {
            random.random_range(0..10).to_string()
          };
          let mut doc = Document::new();
          doc.add(StringField::from_string("id", id.clone(), Store::Yes)?);
          doc.add(LongPoint::new(
            "seq_no",
            [i64::from(seq_no.fetch_add(1, SeqCst))],
          )?);
          if random.random_range(0..10) <= 2 {
            if random.random_bool(0.5) {
              doc.add(NumericDocValuesField::new("soft_deletes", 1));
            }
            writer.soft_update_document(
              Term::from_text("id", id),
              doc,
              vec![NumericDocValuesField::new("soft_deletes", 1).into()],
            )?;
          } else {
            writer.add_document(doc)?;
          }
          if random.random_range(0..100) < 10 {
            let min = retaining_seq_no.load(SeqCst);
            let max = seq_no.load(SeqCst);
            if min < max && random.random_bool(0.5) {
              let _ = retaining_seq_no.compare_exchange(
                min,
                min.wrapping_sub(random.random_range(0..max.wrapping_sub(min))),
                SeqCst,
                SeqCst,
              );
            }
          }
          if random.random_range(0..100) < 10 {
            sm.maybe_refresh_blocking()?;
          }
          if random.random_range(0..100) < 5 {
            writer.commit()?;
          }
          if random.random_range(0..100) < 1 {
            writer
              .force_merge_with_wait(1 + random.random_range(0..10), random.random_bool(0.5))?;
          }
        }
        Ok(())
      }));
    }
    for thread in threads {
      thread
        .join()
        .map_err(|_| LuceneError::illegal_state("indexing thread panicked"))??;
    }
    Ok(())
  });

  let close_result = IOUtils::use_or_suppress_result(sm.close(), writer.close());
  let close_result = IOUtils::use_or_suppress_result(close_result, dir.close());
  IOUtils::use_or_suppress_result(body_result, close_result)
}

#[test]
fn test_max_completed_sequence_number() -> Result<()> {
  let mut random = random();
  {
    let dir = new_directory_shared(&mut random)?;
    let writer = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;
    let body_result = (|| -> Result<()> {
      assert_eq!(1, writer.add_document(Document::new())?);
      assert_eq!(
        2,
        writer.update_document_with_term(Term::from_text("foo", "bar"), Document::new())?
      );
      writer.flush_next_buffer()?;
      assert_eq!(3, writer.commit()?);
      assert_eq!(4, writer.add_document(Document::new())?);
      assert_eq!(4, writer.get_max_completed_sequence_number()?);
      // commit moves seqNo by 2 since there is one DWPT that could still be in-flight
      assert_eq!(6, writer.commit()?);
      assert_eq!(6, writer.get_max_completed_sequence_number()?);
      assert_eq!(7, writer.add_document(Document::new())?);
      directory_reader::open_from_writer(&writer)?.close()?;
      // getReader moves seqNo by 2 since there is one DWPT that could still be in-flight
      assert_eq!(9, writer.get_max_completed_sequence_number()?);
      Ok(())
    })();
    let close_result = IOUtils::use_or_suppress_result(writer.close(), dir.close());
    IOUtils::use_or_suppress_result(body_result, close_result)?;
  }

  let dir = new_directory_shared(&mut random)?;
  let writer = IndexWriter::new(dir.clone(), new_index_writer_config(&mut random)?)?;
  let manager = SearcherManager::from_writer(&writer, Some(SearcherFactory::new()))?;
  let start = CountDownLatch::new(1);
  let num_docs = if cfg!(feature = "nightly") {
    TestUtil::next_int(&mut random, 100, 600)
  } else {
    TestUtil::next_int(&mut random, 10, 60)
  };
  let max_completed_seq_id = AtomicI64::new(-1);
  let num_threads = 2 + random.random_range(0..2);

  let body_result = thread::scope(|scope| -> Result<()> {
    let mut threads = Vec::with_capacity(num_threads);
    for idx in 0..num_threads {
      let start = &start;
      let writer = &writer;
      let manager = &manager;
      let max_completed_seq_id = &max_completed_seq_id;
      threads.push(scope.spawn(move || -> Result<()> {
        start.wait();
        for j in 0..num_docs {
          let mut doc = Document::new();
          let id = format!("{}-{}", idx, j);
          doc.add(StringField::from_string("id", id.clone(), Store::No)?);
          let seq_no = writer.add_document(doc)?;
          if max_completed_seq_id.load(SeqCst) < seq_no {
            let max_completed_sequence_number = writer.get_max_completed_sequence_number()?;
            manager.maybe_refresh_blocking()?;
            max_completed_seq_id.fetch_max(max_completed_sequence_number, SeqCst);
          }
          let acquire = manager.acquire()?;
          let search_result = acquire.search(TermQuery::new(Term::from_text("id", id)), 10);
          let release_result = manager.release(acquire);
          let top_docs = IOUtils::use_or_suppress_result(search_result, release_result)?;
          assert_eq!(1, top_docs.total_hits.value());
        }
        Ok(())
      }));
    }
    start.count_down();
    for thread in threads {
      thread
        .join()
        .map_err(|_| LuceneError::illegal_state("indexing thread panicked"))??;
    }
    Ok(())
  });

  let close_result = IOUtils::use_or_suppress_result(manager.close(), writer.close());
  let close_result = IOUtils::use_or_suppress_result(close_result, dir.close());
  IOUtils::use_or_suppress_result(body_result, close_result)
}

struct Semaphore {
  permits: Mutex<usize>,
  condvar: Condvar,
}

impl Semaphore {
  fn new(permits: usize) -> Self {
    Self {
      permits: Mutex::new(permits),
      condvar: Condvar::new(),
    }
  }

  fn acquire(&self, permits: usize) {
    let mut available = self.permits.lock().expect("semaphore mutex poisoned");
    while *available < permits {
      available = self
        .condvar
        .wait(available)
        .expect("semaphore mutex poisoned");
    }
    *available -= permits;
  }

  fn try_acquire(&self) -> bool {
    let mut available = self.permits.lock().expect("semaphore mutex poisoned");
    if *available == 0 {
      false
    } else {
      *available -= 1;
      true
    }
  }

  fn release(&self, permits: usize) {
    let mut available = self.permits.lock().expect("semaphore mutex poisoned");
    *available += permits;
    self.condvar.notify_all();
  }
}

struct CountDownLatch {
  count: Mutex<usize>,
  condvar: Condvar,
}

impl CountDownLatch {
  fn new(count: usize) -> Self {
    Self {
      count: Mutex::new(count),
      condvar: Condvar::new(),
    }
  }

  fn count_down(&self) {
    let mut count = self.count.lock().expect("latch mutex poisoned");
    if *count > 0 {
      *count -= 1;
      if *count == 0 {
        self.condvar.notify_all();
      }
    }
  }

  fn wait(&self) {
    let mut count = self.count.lock().expect("latch mutex poisoned");
    while *count > 0 {
      count = self.condvar.wait(count).expect("latch mutex poisoned");
    }
  }
}

struct EnsureMaxSeqNoInfoStream {
  wait_ref: Arc<Mutex<Arc<CountDownLatch>>>,
  arrived_ref: Arc<Mutex<Arc<CountDownLatch>>>,
}

impl CloseableRef for EnsureMaxSeqNoInfoStream {
  fn close(&self) -> Result<()> {
    Ok(())
  }
}

impl InfoStream for EnsureMaxSeqNoInfoStream {
  fn message(&self, component: &str, message: &str) -> Result<()> {
    if component == "TP" && message == "DocumentsWriterPerThread addDocuments start" {
      let arrived = self
        .arrived_ref
        .lock()
        .expect("arrived latch mutex poisoned")
        .clone();
      let wait = self
        .wait_ref
        .lock()
        .expect("wait latch mutex poisoned")
        .clone();
      arrived.count_down();
      wait.wait();
    }
    Ok(())
  }

  fn is_enabled(&self, component: &str) -> bool {
    component == "TP"
  }
}

struct EnableTestPointsIndexWriterHooks;

impl IndexWriterHooks for EnableTestPointsIndexWriterHooks {
  fn is_enable_test_points(&self) -> bool {
    true
  }
}

#[test]
fn test_ensure_max_seq_no_is_accurate_during_flush() -> Result<()> {
  let mut random = random();
  let wait_ref = Arc::new(Mutex::new(Arc::new(CountDownLatch::new(0))));
  let arrived_ref = Arc::new(Mutex::new(Arc::new(CountDownLatch::new(0))));
  let stream = EnsureMaxSeqNoInfoStream {
    wait_ref: wait_ref.clone(),
    arrived_ref: arrived_ref.clone(),
  };
  let mut index_writer_config = new_index_writer_config(&mut random)?;
  index_writer_config.set_info_stream(InfoStreamEnum::Custom(Box::new(stream)));
  let dir = new_directory_shared(&mut random)?;
  let writer = IndexWriter::with_hooks(
    dir,
    index_writer_config,
    Some(IndexWriterHooksEnum::custom(
      EnableTestPointsIndexWriterHooks,
    )),
  )?;

  // we produce once DWPT with 1 doc
  writer.add_document(Document::new())?;
  assert_eq!(1, writer.doc_writer.flush_control.per_thread_pool.size());
  let max_completed_sequence_number = writer.get_max_completed_sequence_number()?;
  // safe the seqNo and use the latches to block this DWPT such that a refresh must wait for it
  let wait_latch = Arc::new(CountDownLatch::new(1));
  let arrived_latch = Arc::new(CountDownLatch::new(1));
  *wait_ref.lock().expect("wait latch mutex poisoned") = wait_latch.clone();
  *arrived_ref.lock().expect("arrived latch mutex poisoned") = arrived_latch.clone();

  thread::scope(|scope| -> Result<()> {
    let waiter_thread = scope.spawn(|| writer.add_document(Document::new()));
    arrived_latch.wait();

    let delete_queue = writer.doc_writer.flush_control.delete_queue.lock().clone();
    let refresh_thread = scope.spawn(|| -> Result<()> {
      let reader = directory_reader::open_from_writer(&writer)?;
      reader.close()
    });

    // now we wait until the refresh has swapped the deleted queue and assert that
    // we see an accurate seqId
    while {
      let current_delete_queue = writer.doc_writer.flush_control.delete_queue.lock().clone();
      Arc::ptr_eq(&current_delete_queue, &delete_queue)
    } {
      thread::yield_now(); // busy wait for refresh to swap the queue
    }

    let assertion_result = match writer.get_max_completed_sequence_number() {
      Ok(actual) if actual == max_completed_sequence_number => Ok(()),
      Ok(actual) => Err(LuceneError::illegal_state(format!(
        "expected max completed sequence number {max_completed_sequence_number}, got {actual}"
      ))),
      Err(error) => Err(error),
    };

    wait_latch.count_down();
    let waiter_result = waiter_thread.join().expect("waiter thread panicked");
    let refresh_result = refresh_thread.join().expect("refresh thread panicked");
    assertion_result?;
    waiter_result?;
    refresh_result?;
    Ok(())
  })?;

  assert_eq!(
    max_completed_sequence_number + 2,
    writer.get_max_completed_sequence_number()?
  );
  writer.close()?;
  Ok(())
}

#[test]
fn test_segment_commit_info_id() -> Result<()> {
  let mut random = random();

  {
    let dir = new_directory_shared(&mut random)?;
    let v = {
      let mut iwc = IndexWriterConfig::new()?;
      iwc.set_merge_policy(NoMergePolicy::default());
      let writer = IndexWriter::new(dir.clone(), iwc)?;

      let mut doc = Document::new();
      doc.add(NumericDocValuesField::new("num", 1));
      doc.add(StringField::from_string("id", "1", Store::No)?);
      writer.add_document(doc)?;

      let mut doc = Document::new();
      doc.add(NumericDocValuesField::new("num", 1));
      doc.add(StringField::from_string("id", "2", Store::No)?);
      writer.add_document(doc)?;

      writer.commit()?;
      let segment_commit_infos = SegmentInfos::read_latest_commit(dir.clone())?;
      let mut id = segment_commit_infos.info(0).unwrap().get_id();
      let seg_info_id = segment_commit_infos.info(0).unwrap().info.get_id();

      writer.update_numeric_doc_value(Term::from_text("id", "1"), "num", 2)?;
      writer.commit()?;

      let segment_commit_infos = SegmentInfos::read_latest_commit(dir.clone())?;
      assert_eq!(1, segment_commit_infos.size());
      assert_ne!(
        StringHelper::id_to_string(id),
        StringHelper::id_to_string(segment_commit_infos.info(0).unwrap().get_id())
      );
      assert_eq!(
        StringHelper::id_to_string(Some(seg_info_id)),
        StringHelper::id_to_string(Some(segment_commit_infos.info(0).unwrap().info.get_id()))
      );

      id = segment_commit_infos.info(0).unwrap().get_id();

      writer.add_document(Document::new())?;
      writer.commit()?;

      let segment_commit_infos = SegmentInfos::read_latest_commit(dir.clone())?;
      assert_eq!(2, segment_commit_infos.size());
      assert_eq!(
        StringHelper::id_to_string(id),
        StringHelper::id_to_string(segment_commit_infos.info(0).unwrap().get_id())
      );
      assert_eq!(
        StringHelper::id_to_string(Some(seg_info_id)),
        StringHelper::id_to_string(Some(segment_commit_infos.info(0).unwrap().info.get_id()))
      );

      let mut doc = Document::new();
      doc.add(NumericDocValuesField::new("num", 5));
      doc.add(StringField::from_string("id", "1", Store::No)?);
      writer.update_document_with_term(Term::from_text("id", "1"), doc)?;
      writer.commit()?;

      let segment_commit_infos = SegmentInfos::read_latest_commit(dir.clone())?;
      assert_eq!(3, segment_commit_infos.size());
      assert_ne!(
        StringHelper::id_to_string(id),
        StringHelper::id_to_string(segment_commit_infos.info(0).unwrap().get_id())
      );
      assert_eq!(
        StringHelper::id_to_string(Some(seg_info_id)),
        StringHelper::id_to_string(Some(segment_commit_infos.info(0).unwrap().info.get_id()))
      );

      writer.close()?;
      segment_commit_infos
    };

    {
      let dir2 = new_directory_shared(&mut random)?;
      let mut iwc = IndexWriterConfig::new()?;
      iwc.set_merge_policy(NoMergePolicy::default());
      let writer2 = IndexWriter::new(dir2.clone(), iwc)?;

      writer2.add_indexes_from_directory(std::slice::from_ref(&dir))?;
      writer2.commit()?;

      let infos2 = SegmentInfos::read_latest_commit(dir2)?;
      assert_eq!(infos2.size(), v.size());

      for i in 0..infos2.size() {
        assert_eq!(
          StringHelper::id_to_string(infos2.info(i).unwrap().get_id()),
          StringHelper::id_to_string(v.info(i).unwrap().get_id())
        );
        assert_eq!(
          StringHelper::id_to_string(Some(infos2.info(i).unwrap().info.get_id())),
          StringHelper::id_to_string(Some(v.info(i).unwrap().info.get_id()))
        );
      }

      writer2.close()?;
    }
  }

  let mut ids = HashSet::new();

  for _ in 0..2 {
    let dir = new_directory_shared(&mut random)?;
    let mut iwc = IndexWriterConfig::new()?;
    iwc.set_merge_policy(NoMergePolicy::default());
    let writer = IndexWriter::new(dir.clone(), iwc)?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("num", 1));
    doc.add(StringField::from_string("id", "1", Store::No)?);
    writer.add_document(doc)?;
    writer.commit()?;

    let mut segment_commit_infos = SegmentInfos::read_latest_commit(dir.clone())?;
    let id = StringHelper::id_to_string(segment_commit_infos.info(0).unwrap().get_id());
    assert!(ids.insert(id));

    writer.update_numeric_doc_value(Term::from_text("id", "1"), "num", 2)?;
    writer.commit()?;

    segment_commit_infos = SegmentInfos::read_latest_commit(dir)?;
    let id = StringHelper::id_to_string(segment_commit_infos.info(0).unwrap().get_id());
    assert!(ids.insert(id));

    writer.close()?;
  }

  Ok(())
}

#[test]
fn test_merge_zero_docs_merge_is_closed_once() -> Result<()> {
  let keep_all_segments =
    KeepFullyDeletedSegmentsMergePolicy::new(LogMergePolicy::<LogDocMergePolicy>::log_doc());
  let merge_policy =
    OneMergeWrappingMergePolicy::new(keep_all_segments, MergeFinishedOnceOneMergeUnaryOperator);
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut config = IndexWriterConfig::new()?;
  config.set_merge_policy(merge_policy);
  let writer = IndexWriter::new(dir, config)?;

  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "1", Store::No)?);
  writer.add_document(doc.clone())?;
  writer.flush()?;
  writer.add_document(doc)?;
  writer.flush()?;
  writer.delete_documents_with_terms(vec![Term::from_text("id", "1")])?;
  writer.flush()?;
  assert_eq!(2, writer.get_segment_count());
  assert_eq!(0, writer.get_doc_stats()?.num_docs);
  assert_eq!(2, writer.get_doc_stats()?.max_doc);
  writer.force_merge(1)?;
  writer.close()?;
  Ok(())
}

#[test]
fn test_merge_on_commit_keep_fully_deleted_segments() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut iwc = new_index_writer_config(&mut random)?;
  iwc.set_max_full_flush_merge_wait_millis(30 * 1000);
  iwc.set_merge_policy(KeepFullyDeletedSegmentsMergePolicy::with_full_flush_merges());
  let writer = IndexWriter::new(dir.clone(), iwc)?;

  let mut d = Document::new();
  d.add(StringField::from_string("id", "1", Store::Yes)?);
  writer.add_document(d.clone())?;
  writer.commit()?;
  writer.update_document_with_term(Term::from_text("id", "1"), d)?;
  writer.commit()?;

  let reader = directory_reader::open_from_writer(&writer)?;
  assert_eq!(1, reader.num_docs()?);
  reader.close()?;
  writer.close()?;
  Ok(())
}

#[test]
fn test_pending_num_docs() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let num_docs = random.random_range(0..100);

  {
    let writer = IndexWriter::new(dir.clone(), new_index_writer_config(&mut random)?)?;
    for i in 0..num_docs {
      let mut d = Document::new();
      d.add(StringField::from_string("id", i.to_string(), Store::Yes)?);
      writer.add_document(d)?;
      assert_eq!(i as i64 + 1, writer.get_pending_num_docs());
    }
    assert_eq!(num_docs as i64, writer.get_pending_num_docs());
    writer.flush()?;
    assert_eq!(num_docs as i64, writer.get_pending_num_docs());
    writer.close()?;
  }

  {
    let writer = IndexWriter::new(dir.clone(), new_index_writer_config(&mut random)?)?;
    assert_eq!(num_docs as i64, writer.get_pending_num_docs());
    writer.close()?;
  }
  Ok(())
}

#[test]
fn test_index_writer_blocks_on_stall() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let writer = IndexWriter::new(dir.clone(), new_index_writer_config(&mut random)?)?;
  let stall_control = &writer.get_docs_writer().flush_control.stall_control;
  stall_control.update_stalled(true);
  let num_threads = random.random_range(0..3) + 1;
  let num_threads_completed = AtomicI64::new(0);

  thread::scope(|scope| -> Result<()> {
    let mut threads = Vec::new();
    for _ in 0..num_threads {
      threads.push(scope.spawn(|| -> Result<()> {
        let mut d = Document::new();
        d.add(StringField::from_string("id", 0.to_string(), Store::Yes)?);
        writer.add_document(d)?;
        num_threads_completed.fetch_add(1, SeqCst);
        Ok(())
      }));
    }

    let result = {
      for _ in 0..10 {
        while stall_control.get_num_waiting() != num_threads {
          // wait for all threads to be stalled again
          assert_eq!(0, writer.get_pending_num_docs());
          assert_eq!(0, num_threads_completed.load(SeqCst));
          thread::yield_now();
        }
      }
      Ok(())
    };

    stall_control.update_stalled(false);
    for thread in threads {
      thread.join().expect("thread panicked")?;
    }
    result
  })?;

  writer.commit()?;
  assert_eq!(num_threads, writer.get_doc_stats()?.max_doc);
  writer.close()?;
  Ok(())
}

#[test]
fn test_get_field_names() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  {
    let writer = IndexWriter::new(dir.clone(), new_index_writer_config(&mut random)?)?;
    let mut field_types = HashMap::new();

    assert_eq!(HashSet::<String>::new(), writer.get_field_names());

    add_doc_with_field(&mut random, &writer, "f1", &mut field_types)?;
    assert_eq!(HashSet::from(["f1".to_string()]), writer.get_field_names());

    let field_set = writer.get_field_names();

    add_doc_with_field(&mut random, &writer, "f2", &mut field_types)?;
    assert_eq!(
      HashSet::from(["f1".to_string(), "f2".to_string()]),
      writer.get_field_names()
    );
    assert_eq!(HashSet::from(["f1".to_string()]), field_set);

    // flush should not change field names
    writer.flush()?;
    assert_eq!(
      HashSet::from(["f1".to_string(), "f2".to_string()]),
      writer.get_field_names()
    );

    // commit should not change field names
    writer.commit()?;
    assert_eq!(
      HashSet::from(["f1".to_string(), "f2".to_string()]),
      writer.get_field_names()
    );

    writer.close()?;
  }

  // reopen writer — should detect committed fields
  let mock = MockAnalyzer::new(&mut random);
  let config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let writer = IndexWriter::new(dir.clone(), config)?;
  assert_eq!(
    HashSet::from(["f1".to_string(), "f2".to_string()]),
    writer.get_field_names()
  );

  writer.delete_all()?;
  assert_eq!(HashSet::<String>::new(), writer.get_field_names());

  writer.close()?;
  Ok(())
}

#[test]
fn test_parent_and_soft_deletes_are_the_same() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut index_writer_config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  index_writer_config.set_soft_deletes_field("foo");
  index_writer_config.set_parent_field("foo");

  let err = IndexWriter::new(dir, index_writer_config);
  match err {
    Ok(_) => unreachable!("expected IllegalArgument error"),
    Err(err) => {
      assert!(matches!(err, LuceneError::IllegalArgument(_)));
      assert_eq!(
        "parent document and soft-deletes field can't be the same field \"foo\"",
        err.to_string()
      );
    },
  }

  Ok(())
}

#[test]
fn test_parent_field_existing_index() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;

  {
    let mock = MockAnalyzer::new(&mut random);
    let iwc = IndexWriterConfig::with_analyzer(mock)?;
    let writer = IndexWriter::new(dir.clone(), iwc)?;

    let mut field_to_type = HashMap::new();
    let mut d = Document::new();
    d.add(new_text_field(
      &mut random,
      "f",
      "a",
      Store::No,
      &mut field_to_type,
    )?);
    writer.add_document(d)?;
    writer.close()?;
  }

  {
    let mock = MockAnalyzer::new(&mut random);
    let mut iwc = IndexWriterConfig::with_analyzer(mock)?;
    iwc.set_open_mode(OpenMode::Append);
    iwc.set_parent_field("foo");

    let err = IndexWriter::new(dir.clone(), iwc);
    match err {
      Ok(_) => unreachable!("expected IllegalArgument error"),
      Err(err) => {
        assert!(matches!(err, LuceneError::IllegalArgument(_)));
        assert_eq!(
          "can't add a parent field to an already existing index without a parent field",
          err.to_string()
        );
      },
    }
  }

  {
    let mock = MockAnalyzer::new(&mut random);
    let mut iwc = IndexWriterConfig::with_analyzer(mock)?;
    iwc.set_open_mode(OpenMode::CreateOrAppend);
    iwc.set_parent_field("foo");

    let err = IndexWriter::new(dir.clone(), iwc);
    match err {
      Ok(_) => unreachable!("expected IllegalArgument error"),
      Err(err) => {
        assert!(matches!(err, LuceneError::IllegalArgument(_)));
        assert_eq!(
          "can't add a parent field to an already existing index without a parent field",
          err.to_string()
        );
      },
    }
  }

  {
    let mock = MockAnalyzer::new(&mut random);
    let mut iwc = IndexWriterConfig::with_analyzer(mock)?;
    iwc.set_open_mode(OpenMode::Create);
    iwc.set_parent_field("foo");

    let writer = IndexWriter::new(dir, iwc)?;
    writer.add_document(Document::new())?;
    writer.close()?;
  }

  Ok(())
}

#[test]
fn test_index_with_parent_field_is_congruent() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;

  {
    let mock = MockAnalyzer::new(&mut random);
    let mut iwc = IndexWriterConfig::with_analyzer(mock)?;
    iwc.set_parent_field("parent");
    let writer = IndexWriter::new(dir.clone(), iwc)?;

    if random.random_bool(0.5) {
      let mut child1 = Document::new();
      child1.add(StringField::from_string("id", 1.to_string(), Store::Yes)?);
      let mut child2 = Document::new();
      child2.add(StringField::from_string("id", 1.to_string(), Store::Yes)?);
      let mut parent = Document::new();
      parent.add(StringField::from_string("id", 1.to_string(), Store::Yes)?);
      writer.add_documents(vec![child1.clone(), child2.clone(), parent.clone()])?;
      writer.flush()?;
      if random.random_bool(0.5) {
        writer.add_documents(vec![child1, child2, parent])?;
      }
    } else {
      writer.add_document(Document::new())?;
    }
    writer.commit()?;
    writer.close()?;
  }

  {
    let mock = MockAnalyzer::new(&mut random);
    let mut config = IndexWriterConfig::with_analyzer(mock)?;
    config.set_parent_field("someOtherField");

    let err = IndexWriter::new(dir.clone(), config);
    match err {
      Ok(writer) => {
        writer.close()?;
        panic!("expected IllegalArgument error");
      },
      Err(err) => {
        assert!(matches!(err, LuceneError::IllegalArgument(_)));
        assert_eq!(
          "can't add field [parent] as parent document field; this IndexWriter is configured with [someOtherField] as parent document field",
          err.to_string()
        );
      },
    }
  }

  {
    let mock = MockAnalyzer::new(&mut random);
    let config = IndexWriterConfig::with_analyzer(mock)?;

    let err = IndexWriter::new(dir, config);
    match err {
      Ok(writer) => {
        writer.close()?;
        panic!("expected IllegalArgument error");
      },
      Err(err) => {
        assert!(matches!(err, LuceneError::IllegalArgument(_)));
        assert_eq!(
          "can't add field [parent] as parent document field; this IndexWriter has no parent document field configured",
          err.to_string()
        );
      },
    }
  }

  Ok(())
}

#[test]
fn test_parent_field_is_already_used() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;

  {
    let mock = MockAnalyzer::new(&mut random);
    let iwc = IndexWriterConfig::with_analyzer(mock)?;
    let writer = IndexWriter::new(dir.clone(), iwc)?;

    let mut doc = Document::new();
    doc.add(StringField::from_string(
      "parent",
      1.to_string(),
      Store::Yes,
    )?);
    writer.add_document(doc)?;
    writer.commit()?;
    writer.close()?;
  }

  let mock = MockAnalyzer::new(&mut random);
  let mut config = IndexWriterConfig::with_analyzer(mock)?;
  config.set_parent_field("parent");

  let err = IndexWriter::new(dir, config);

  assert!(err.is_err());

  let err = match err {
    Ok(_) => unreachable!(),
    Err(err) => err,
  };

  assert!(matches!(err, LuceneError::IllegalArgument(_)));
  assert_eq!(
    "can't add [parent] as non parent document field; this IndexWriter is configured with [parent] as parent document field",
    err.to_string()
  );

  Ok(())
}

#[test]
fn test_parent_field_empty_index() -> Result<()> {
  let mut random = random();

  let dir = Arc::new(new_mock_directory(&mut random)?);

  {
    let mock = MockAnalyzer::new(&mut random);
    let mut iwc = IndexWriterConfig::with_analyzer(mock)?;
    iwc.set_parent_field("parent");
    let writer = IndexWriter::new(dir.clone(), iwc)?;
    writer.commit()?;
    writer.close()?;
  }

  {
    let mock = MockAnalyzer::new(&mut random);
    let mut iwc2 = IndexWriterConfig::with_analyzer(mock)?;
    iwc2.set_parent_field("parent");
    let writer = IndexWriter::new(dir, iwc2)?;
    writer.commit()?;
    writer.close()?;
  }

  Ok(())
}

#[test]
fn test_doc_values_mixed_skipping_index() -> Result<()> {
  let mut random = random();
  {
    let dir = new_directory_shared(&mut random)?;
    let mock = MockAnalyzer::new(&mut random);
    let iwc = IndexWriterConfig::with_analyzer(mock)?;
    let writer = IndexWriter::new(dir, iwc)?;

    let mut doc1 = Document::new();
    doc1.add(SortedNumericDocValuesField::indexed_field(
      "test",
      random.random(),
    ));
    writer.add_document(doc1)?;

    let mut doc2 = Document::new();
    doc2.add(SortedNumericDocValuesField::new("test", random.random()));

    let err = writer.add_document(doc2);
    assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
    let err = err.unwrap_err();
    assert_eq!(
      "Inconsistency of field data structures across documents for field [test] of doc [1]. doc values skip index type: expected 'Range', but it has 'None'.",
      err.to_string()
    );

    writer.close()?;
  }

  {
    let dir = new_directory_shared(&mut random)?;
    let mock = MockAnalyzer::new(&mut random);
    let iwc = IndexWriterConfig::with_analyzer(mock)?;
    let writer = IndexWriter::new(dir, iwc)?;

    let mut doc1 = Document::new();
    doc1.add(SortedSetDocValuesField::new(
      "test",
      TestUtil::random_binary_term(&mut random),
    ));
    writer.add_document(doc1)?;

    let mut doc2 = Document::new();
    doc2.add(SortedSetDocValuesField::indexed_field(
      "test",
      TestUtil::random_binary_term(&mut random),
    ));

    let err = writer.add_document(doc2);
    assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
    let err = err.unwrap_err();
    assert_eq!(
      "Inconsistency of field data structures across documents for field [test] of doc [1]. doc values skip index type: expected 'None', but it has 'Range'.",
      err.to_string()
    );

    writer.close()?;
  }

  Ok(())
}

#[test]
fn test_doc_values_skipping_index_without_doc_values() -> Result<()> {
  let mut random = random();

  for doc_values_type in [DocValuesType::None, DocValuesType::Binary] {
    let mut field_type = FieldType::new();
    field_type.set_stored(true)?;
    field_type.set_doc_values_type(doc_values_type)?;
    field_type.set_doc_values_skip_index_type(DocValuesSkipIndexType::Range)?;
    field_type.freeze();
    let dir = new_mock_directory(&mut random)?;
    let mock = MockAnalyzer::new(&mut random);
    let iwc = IndexWriterConfig::with_analyzer(mock)?;
    let writer = IndexWriter::new(Arc::new(dir.clone()), iwc)?;

    let mut doc1 = Document::new();
    doc1.add(Field::from_binary("test", vec![0u8; 10], field_type)?);

    let err = writer.add_document(doc1);
    assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
    assert!(
      err
        .unwrap_err()
        .to_string()
        .starts_with("field 'test' cannot have docValuesSkipIndexType=Range")
    );

    writer.close()?;
  }

  Ok(())
}

// Make sure we can flush segment w/ norms, then add empty doc (no norms) and flush
struct MockIndexWriter {
  after_was_called: Arc<AtomicBool>,
  before_was_called: Arc<AtomicBool>,
}
impl MockIndexWriter {
  fn new() -> Self {
    MockIndexWriter {
      after_was_called: Arc::new(AtomicBool::new(false)),
      before_was_called: Arc::new(AtomicBool::new(false)),
    }
  }
}

struct NegativePositionsTokenStream {
  attrs: Attributes,
  terms: [&'static str; 3],
  upto: usize,
  first: bool,
}

impl NegativePositionsTokenStream {
  fn new() -> Self {
    Self {
      attrs: Attributes::default(),
      terms: ["a", "b", "c"],
      upto: 0,
      first: true,
    }
  }
}

impl crate::core::util::close::Closeable for NegativePositionsTokenStream {}

impl TokenStream for NegativePositionsTokenStream {
  fn increment_token(&mut self) -> Result<bool> {
    if self.upto == self.terms.len() {
      return Ok(false);
    }

    self.attrs.clear_attributes()?;
    self.attrs.append_str(Some(self.terms[self.upto]))?;
    self
      .attrs
      .set_position_increment(if self.first { 0 } else { 1 })?;
    self.first = false;
    self.upto += 1;
    Ok(true)
  }

  fn end(&mut self) -> Result<()> {
    self.default_end()
  }

  fn reset(&mut self) -> Result<()> {
    self.upto = 0;
    self.first = true;
    Ok(())
  }

  fn get_attribute_source(&self) -> &Attributes {
    &self.attrs
  }

  fn get_attribute_source_mut(&mut self) -> &mut Attributes {
    &mut self.attrs
  }
}

impl IndexWriterHooks for MockIndexWriter {
  fn do_after_flush(&self) -> Result<()> {
    self.after_was_called.store(true, SeqCst);
    Ok(())
  }

  fn do_before_flush(&self) -> Result<()> {
    self.before_was_called.store(true, SeqCst);
    Ok(())
  }
}

fn assert_hard_live_docs<D>(writer: &Arc<IndexWriter<D>>, unique_docs: &HashSet<i32>) -> Result<()>
where
  D: Directory + 'static,
{
  let reader = directory_reader::open_from_writer(writer)?;
  assert_eq!(unique_docs.len() as i32, reader.num_docs()?);
  let context = (&reader).get_context()?;
  for ctx in context.leaves()? {
    let sr = ctx.reader();
    if let Some(hard_live_docs) = sr.get_hard_live_docs()? {
      let id = LeafReader::terms(sr, "id")?.unwrap();
      let mut iterator = id.iterator()?;
      let live_docs = sr.get_live_docs()?.unwrap();
      for d_id in unique_docs {
        let must_be_hard_deleted = d_id % 2 == 0;
        if iterator.seek_exact(&BytesRef::from_string(&d_id.to_string()))? {
          let mut postings = iterator.postings(None)?;
          while postings.next_doc()? != NO_MORE_DOCS {
            let doc_id = postings.doc_id() as usize;
            if live_docs.get(doc_id)? {
              assert!(hard_live_docs.get(doc_id)?);
            } else if must_be_hard_deleted {
              assert!(!hard_live_docs.get(doc_id)?);
            } else {
              assert!(hard_live_docs.get(doc_id)?);
            }
          }
        }
      }
    }
  }
  reader.close()?;
  Ok(())
}

fn add_doc_with_field<D, R>(
  random: &mut R,
  writer: &IndexWriter<D>,
  field: &str,
  field_types: &mut HashMap<String, FieldType>,
) -> Result<()>
where
  D: Directory + 'static,
  R: Rng + ?Sized,
{
  let mut doc = Document::new();
  let stored_text_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_STORED)?;
  doc.add(new_field(
    random,
    field,
    "value",
    &stored_text_type,
    field_types,
  )?);
  let _ = writer.add_document(doc)?;
  Ok(())
}
