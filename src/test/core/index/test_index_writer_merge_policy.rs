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
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::document::string_field::StringField;
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::concurrent_merge_scheduler::{
  ConcurrentMergeScheduler, ConcurrentMergeSchedulerHook,
};
use crate::core::index::directory_reader;
use crate::core::index::index_reader::{Identity, IndexReader};
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::Inner;
use crate::core::index::index_writer::{
  IndexWriter, IndexWriterHooks, IndexWriterHooksEnum, SOURCE, SOURCE_MERGE,
};
use crate::core::index::index_writer_config::OpenMode;
use crate::core::index::index_writer_event_listener::IndexWriterEventListenerEnum;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::log_byte_size_merge_policy::LogByteSizeMergePolicy;
use crate::core::index::log_merge_policy::LogMergePolicy;
use crate::core::index::merge_policy::{
  DefaultMergeSpecification, MergeContext, MergePolicy, MergePolicyBase, MergePolicyEnum,
  MergeSpecification, OneMerge, size,
};
use crate::core::index::merge_scheduler::MergeSchedulerEnum;
use crate::core::index::merge_trigger::MergeTrigger;
use crate::core::index::no_merge_policy::NoMergePolicy;
use crate::core::index::one_merge_wrapping_merge_policy::{
  NewOneMergeUnaryOperator, OneMergeWrappingMergePolicy,
};
use crate::core::index::segment_commit_info::SegmentCommitInfo;
use crate::core::index::segment_infos::SegmentInfos;
use crate::core::index::segment_reader::DefaultLeafReader;
use crate::core::index::serial_merge_scheduler::SerialMergeScheduler;
use crate::core::index::soft_deletes_directory_reader_wrapper::SoftDeletesDirectoryReaderWrapper;
use crate::core::index::soft_deletes_retention_merge_policy::SoftDeletesRetentionMergePolicy;
use crate::core::index::term::Term;
use crate::core::index::tiered_merge_policy::{SegmentDocAndID, TieredMergePolicy};
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::search::index_searcher::{self, IndexSearcher};
use crate::core::search::match_all_docs_query::MatchAllDocsQuery;
use crate::core::search::term_query::TermQuery;
use crate::core::store::data_input::DataInput;
use crate::test_framework::core::util::lucene_test_case::{
  create_temp_dir_with_prefix, new_directory_shared, new_fs_directory, new_index_writer_config,
  new_index_writer_config_with_analyzer, new_log_merge_policy_with_merge_factor, new_merge_policy,
  new_string_field, new_text_field, random,
};

use crate::core::store::directory::Directory;
use crate::core::store::index_input::IndexInput;
use crate::core::store::io_context::IOContext;
use crate::core::store::random_access_input::RandomAccessInputWrapper;
use crate::core::util::HasIdentity;
use crate::core::util::clone::TryClone;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::index::mock_index_writer_event_listener::MockIndexWriterEventListener;
use crate::test_framework::core::index::test_concurrent_merge_scheduler::CountDownLatch;
use crate::test_framework::core::index::test_index_writer_merge_policy::{
  ForceMergeDvUpdateMergePolicy, ForceMergeDvUpdateOneMergeUnaryOperator,
  LatchedSerialMergeScheduler, MergeDvUpdateFileOnCommitConcurrentMergeScheduler,
  MergeDvUpdateFileOnGetReaderConcurrentMergeScheduler, SetDiagnosticsMergePolicy, TestLatch,
};
use rand::{Rng, RngExt};
use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter};
use std::io::{Error, ErrorKind};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::thread;
use std::time::Duration;

#[allow(dead_code)] // for quick search
struct TestIndexWriterMergePolicy;

pub use crate::test_framework::core::index::merge_policy::{
  MergeOnXMergePolicy, MockMergePolicy, OnlyForceMergeMergePolicy,
};

#[test]
fn test_normal_case() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config
    .set_max_buffered_docs(10)
    .set_merge_policy(MockMergePolicy::default());
  let writer = IndexWriter::new(dir, config)?;
  let mut field_types = HashMap::new();

  for _ in 0..100 {
    add_doc(&mut random, &writer, &mut field_types)?;
    check_invariants(&writer)?;
  }

  writer.close()?;
  Ok(())
}

#[test]
fn test_no_over_merge() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config
    .set_max_buffered_docs(10)
    .set_merge_policy(MockMergePolicy::default());
  let writer = IndexWriter::new(dir, config)?;
  let mut field_types = HashMap::new();

  let mut no_over_merge = false;
  for _ in 0..100 {
    add_doc(&mut random, &writer, &mut field_types)?;
    check_invariants(&writer)?;
    if writer.get_num_buffered_documents() as usize + writer.get_segment_count() >= 18 {
      no_over_merge = true;
    }
  }
  assert!(no_over_merge);

  writer.close()?;
  Ok(())
}

#[test]
fn test_force_flush() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut merge_policy = MockMergePolicy::default();
  merge_policy.set_merge_factor(10);
  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config
    .set_max_buffered_docs(10)
    .set_merge_policy(merge_policy);
  let writer = IndexWriter::new(dir, config)?;
  let mut field_types = HashMap::new();

  for _ in 0..100 {
    add_doc(&mut random, &writer, &mut field_types)?;
    writer.flush()?;
  }

  writer.close()?;
  Ok(())
}

#[test]
fn test_merge_factor_change() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config
    .set_max_buffered_docs(10)
    .set_merge_policy(MockMergePolicy::default())
    .set_merge_scheduler(SerialMergeScheduler::new());
  let writer = IndexWriter::new(dir, config)?;
  let mut field_types = HashMap::new();

  for _ in 0..250 {
    add_doc(&mut random, &writer, &mut field_types)?;
    check_invariants(&writer)?;
  }

  if let MergePolicyEnum::Mock(merge_policy) = writer.get_config_mut().get_merge_policy_mut() {
    merge_policy.set_merge_factor(5);
  }

  for _ in 0..10 {
    add_doc(&mut random, &writer, &mut field_types)?;
  }
  check_invariants(&writer)?;

  writer.close()?;
  Ok(())
}

#[cfg(feature = "nightly")]
#[test]
#[ignore = "nightly"]
fn test_max_buffered_docs_change() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut field_types = HashMap::new();

  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config
    .set_max_buffered_docs(101)
    .set_merge_policy(MockMergePolicy::default())
    .set_merge_scheduler(SerialMergeScheduler::new());
  let mut writer = IndexWriter::new(dir.clone(), config)?;

  for i in 1..=100 {
    for _ in 0..i {
      add_doc(&mut random, &writer, &mut field_types)?;
      check_invariants(&writer)?;
    }
    writer.close()?;

    let mock = MockAnalyzer::new(&mut random);
    let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
    config
      .set_open_mode(OpenMode::Append)
      .set_max_buffered_docs(101)
      .set_merge_policy(MockMergePolicy::default())
      .set_merge_scheduler(SerialMergeScheduler::new());
    writer = IndexWriter::new(dir.clone(), config)?;
  }

  writer.close()?;
  let mut merge_policy = MockMergePolicy::default();
  merge_policy.set_merge_factor(10);
  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config
    .set_open_mode(OpenMode::Append)
    .set_max_buffered_docs(10)
    .set_merge_policy(merge_policy)
    .set_merge_scheduler(SerialMergeScheduler::new());
  writer = IndexWriter::new(dir, config)?;

  for _ in 0..100 {
    add_doc(&mut random, &writer, &mut field_types)?;
  }
  check_invariants(&writer)?;

  for _ in 100..1000 {
    add_doc(&mut random, &writer, &mut field_types)?;
  }
  writer.commit()?;
  writer.wait_for_merges()?;
  writer.commit()?;
  check_invariants(&writer)?;

  writer.close()?;
  Ok(())
}

#[test]
fn test_merge_doc_count_0() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut field_types = HashMap::new();

  let mut merge_policy = MockMergePolicy::default();
  merge_policy.set_merge_factor(100);
  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config
    .set_max_buffered_docs(10)
    .set_merge_policy(merge_policy);
  let writer = IndexWriter::new(dir.clone(), config)?;

  for _ in 0..250 {
    add_doc(&mut random, &writer, &mut field_types)?;
    check_invariants(&writer)?;
  }
  writer.close()?;

  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config.set_merge_policy(NoMergePolicy::default());
  let writer = IndexWriter::new(dir.clone(), config)?;
  writer.delete_documents_with_terms(vec![Term::from_text("content", "aaa")])?;
  writer.close()?;

  let mut merge_policy = MockMergePolicy::default();
  merge_policy.set_merge_factor(5);
  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config
    .set_open_mode(OpenMode::Append)
    .set_max_buffered_docs(10)
    .set_merge_policy(merge_policy)
    .set_merge_scheduler(ConcurrentMergeScheduler::new());
  let writer = IndexWriter::new(dir, config)?;

  for _ in 0..10 {
    add_doc(&mut random, &writer, &mut field_types)?;
  }
  writer.commit()?;
  writer.wait_for_merges()?;
  writer.commit()?;
  check_invariants(&writer)?;
  assert_eq!(10, writer.get_doc_stats()?.max_doc);

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

fn check_invariants<D>(writer: &IndexWriter<D>) -> Result<()>
where
  D: Directory + 'static,
{
  writer.wait_for_merges()?;
  let max_buffered_docs = writer.get_config().get_max_buffered_docs();
  let merge_factor = match writer.get_config().get_merge_policy() {
    MergePolicyEnum::Mock(merge_policy) => merge_policy.get_merge_factor(),
    _ => unreachable!(),
  };

  let ram_segment_count = writer.get_num_buffered_documents();
  assert!(ram_segment_count < max_buffered_docs);

  let segment_count = writer.get_segment_count() as i32;
  let mut lower_bound = i32::MAX;
  for i in 0..segment_count {
    lower_bound = lower_bound.min(writer.max_doc(i));
  }
  let upper_bound = lower_bound.wrapping_mul(merge_factor);

  let mut segments_across_levels = 0;
  while segments_across_levels < segment_count {
    let mut segments_on_current_level = 0;
    for i in 0..segment_count {
      let doc_count = writer.max_doc(i);
      if doc_count >= lower_bound && doc_count < upper_bound {
        segments_on_current_level += 1;
      }
    }
    assert!(segments_on_current_level < merge_factor);
    segments_across_levels += segments_on_current_level;
  }
  Ok(())
}

/// Port of Java TestIndexWriterMergePolicy.assertSetters(MergePolicy)
const EPSILON: f64 = 1e-14;

fn assert_setters<D, P>(lmp: &mut P) -> Result<()>
where
  D: Directory,
  P: MergePolicy<D>,
{
  let base = lmp.get_base_mut();
  base.set_max_cfs_segment_size_mb(2.0)?;
  assert!((base.get_max_cfs_segment_size_mb() - 2.0).abs() < EPSILON);

  base.set_max_cfs_segment_size_mb(f64::INFINITY)?;
  assert!(
    (base.get_max_cfs_segment_size_mb() - (i64::MAX as f64 / 1024.0 / 1024.0)).abs()
      < EPSILON * i64::MAX as f64
  );

  base.set_max_cfs_segment_size_mb(i64::MAX as f64 / 1024.0 / 1024.0)?;
  assert!(
    (base.get_max_cfs_segment_size_mb() - (i64::MAX as f64 / 1024.0 / 1024.0)).abs()
      < EPSILON * i64::MAX as f64
  );

  assert!(base.set_max_cfs_segment_size_mb(-2.0).is_err());

  Ok(())
}

#[test]
fn test_setters() -> Result<()> {
  let mut lmp = LogMergePolicy::<LogByteSizeMergePolicy>::log_bytes_size();
  assert_setters::<crate::core::store::dummy::dummy_directory::DummyDirectory, _>(&mut lmp)?;

  let mut mock = MockMergePolicy::default();
  assert_setters::<crate::core::store::dummy::dummy_directory::DummyDirectory, _>(&mut mock)?;

  Ok(())
}

#[test]
fn test_merge_on_commit() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  // First writer: no merge policy, add 5 docs (each flushed)
  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config.set_merge_policy(NoMergePolicy::default());
  let first_writer = IndexWriter::new(dir.clone(), config)?;

  let mut field_types = HashMap::new();
  for _ in 0..5 {
    crate::test_framework::core::index::test_index_writer::add_doc(
      &mut random,
      &first_writer,
      &mut field_types,
    )?;
    first_writer.flush()?;
  }

  // Check 5 leaf segments
  {
    let first_reader = directory_reader::open_from_writer(&first_writer)?;
    let first_ctx = first_reader.get_context()?;
    assert_eq!(5, first_ctx.leaves()?.len());
  }
  first_writer.close()?;

  // Second writer: MergeOnX with COMMIT trigger
  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config
    .set_merge_policy(MergeOnXMergePolicy::new(
      new_merge_policy(&mut random)?,
      MergeTrigger::Commit,
    ))
    .set_max_full_flush_merge_wait_millis(i64::MAX);
  let writer_with_merge_policy = IndexWriter::new(dir.clone(), config)?;

  {
    let unmerged_reader = directory_reader::open_from_writer(&writer_with_merge_policy)?;
    let unmerged_ctx = unmerged_reader.get_context()?;
    let leaf_count = unmerged_ctx.leaves()?.len();
    assert_eq!(5, leaf_count);
  }

  // Commit triggers merge
  writer_with_merge_policy.commit()?;
  assert_eq!(1, writer_with_merge_policy.get_segment_count());

  {
    let merged_reader = directory_reader::open_from_writer(&writer_with_merge_policy)?;
    let merged_ctx = merged_reader.get_context()?;
    assert_eq!(1, merged_ctx.leaves()?.len());
  }

  let reader = directory_reader::open_from_writer(&writer_with_merge_policy)?;
  assert_eq!(5, reader.num_docs()?);
  let searcher = index_searcher::from_reader(reader)?;
  assert_eq!(5, searcher.count(MatchAllDocsQuery::new())?);

  searcher.reader_context.reader().close()?;
  writer_with_merge_policy.close()?;
  Ok(())
}

// Test basic semantics of merge on commit and events recording invocation
#[test]
fn test_merge_on_commit_with_event_listener() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config.set_merge_policy(NoMergePolicy::default());
  let first_writer = IndexWriter::new(dir.clone(), config)?;

  let mut field_types = HashMap::new();
  for _ in 0..5 {
    crate::test_framework::core::index::test_index_writer::add_doc(
      &mut random,
      &first_writer,
      &mut field_types,
    )?;
    first_writer.flush()?;
  }

  let first_reader = directory_reader::open_from_writer(&first_writer)?;
  assert_eq!(5, (&first_reader).get_context()?.leaves()?.len());
  first_reader.close()?;
  first_writer.close()?; // When this writer closes, it does not merge on commit.

  let event_listener = MockIndexWriterEventListener::new();

  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config
    .set_merge_policy(MergeOnXMergePolicy::new(
      new_merge_policy(&mut random)?,
      MergeTrigger::Commit,
    ))
    .set_max_full_flush_merge_wait_millis(i64::MAX)
    .set_index_writer_event_listener(IndexWriterEventListenerEnum::custom(event_listener.clone()));
  let writer_with_merge_policy = IndexWriter::new(dir.clone(), config)?;

  // No changes. Refresh doesn't trigger a merge.
  let unmerged_reader = directory_reader::open_from_writer(&writer_with_merge_policy)?;
  assert_eq!(5, (&unmerged_reader).get_context()?.leaves()?.len());
  unmerged_reader.close()?;

  assert!(!event_listener.is_events_recorded());
  writer_with_merge_policy.commit()?; // Do merge on commit.
  assert_eq!(1, writer_with_merge_policy.get_segment_count());
  assert!(event_listener.is_events_recorded());

  writer_with_merge_policy.close()?;
  Ok(())
}

#[test]
fn test_carry_over_new_deletes_on_commit() -> Result<()> {
  struct CarryOverNewDeletesHooks {
    wait_for_merge: TestLatch,
    wait_for_update: TestLatch,
  }

  impl IndexWriterHooks for CarryOverNewDeletesHooks {
    fn do_before_merge(&self, _merge: &crate::core::index::merge_policy::MergeStat) -> Result<()> {
      self.wait_for_merge.count_down();
      await_latch(&self.wait_for_update, "update did not finish before merge")
    }
  }

  let mut random = random();
  let directory = new_directory_shared(&mut random)?;
  let use_soft_deletes = random.random_bool(0.5);
  let wait_for_merge = TestLatch::new();
  let wait_for_update = TestLatch::new();
  let mut config = new_index_writer_config(&mut random)?;
  config
    .set_merge_policy(MergeOnXMergePolicy::new(
      NoMergePolicy::default().into(),
      MergeTrigger::Commit,
    ))
    .set_max_full_flush_merge_wait_millis(30 * 1000)
    .set_soft_deletes_field("soft_delete")
    .set_max_buffered_docs(i32::MAX)
    .set_ram_buffer_size_mb(100.0)
    .set_merge_scheduler(ConcurrentMergeScheduler::new());
  let writer = IndexWriter::with_hooks(
    directory.clone(),
    config,
    Some(IndexWriterHooksEnum::custom(CarryOverNewDeletesHooks {
      wait_for_merge: wait_for_merge.clone(),
      wait_for_update: wait_for_update.clone(),
    })),
  )?;

  writer.add_document(id_doc("1")?)?;
  writer.flush()?;
  writer.add_document(id_doc("2")?)?;
  let add_three_docs = random.random_bool(0.5);
  let expected_num_docs = if add_three_docs {
    writer.add_document(id_doc("3")?)?;
    3
  } else {
    2
  };

  let thread_writer = writer.clone();
  let thread_wait_for_merge = wait_for_merge.clone();
  let thread_wait_for_update = wait_for_update.clone();
  let handle = thread::spawn(move || -> Result<()> {
    let update_result = (|| -> Result<()> {
      await_latch(&thread_wait_for_merge, "commit merge did not start")?;
      if use_soft_deletes {
        thread_writer.soft_update_document(
          Term::from_text("id", "2"),
          id_doc("2")?,
          soft_delete_marker(),
        )?;
      } else {
        thread_writer.update_document_with_term(Term::from_text("id", "2"), id_doc("2")?)?;
      }
      thread_writer.flush()?;
      Ok(())
    })();
    thread_wait_for_update.count_down();
    update_result
  });

  writer.commit()?;
  join_result(handle)?;

  let reader = SoftDeletesDirectoryReaderWrapper::new(
    directory_reader::open(directory.clone())?,
    "soft_delete",
  )?;
  assert_eq!(expected_num_docs, reader.num_docs()?);
  assert_eq!(
    expected_num_docs,
    reader.max_doc()?,
    "we should not have any deletes"
  );
  reader.close()?;

  let reader = directory_reader::open_from_writer(&writer)?;
  assert_eq!(expected_num_docs, reader.num_docs()?);
  assert_eq!(
    expected_num_docs + 1,
    reader.max_doc()?,
    "we should have one delete"
  );
  reader.close()?;

  writer.close()?;
  directory.as_ref().close()
}

fn id_doc(id: &str) -> Result<Document> {
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", id, Store::No)?);
  Ok(doc)
}

fn soft_delete_marker() -> Vec<crate::core::document::fields::Fields> {
  vec![NumericDocValuesField::new("soft_delete", 1).into()]
}

fn join_result(handle: thread::JoinHandle<Result<()>>) -> Result<()> {
  handle
    .join()
    .map_err(|payload| LuceneError::tragedy_from_panic("test thread panicked", payload.as_ref()))?
}

fn await_latch(latch: &TestLatch, message: &str) -> Result<()> {
  if latch.wait_timeout(Duration::from_secs(30)) {
    Ok(())
  } else {
    Err(LuceneError::illegal_state(message))
  }
}

fn abort_merge_on_x(use_get_reader: bool) -> Result<()> {
  let mut random = random();
  let directory = new_directory_shared(&mut random)?;
  let wait_for_merge = TestLatch::new();
  let wait_for_delete_all = TestLatch::new();
  let mut config = new_index_writer_config(&mut random)?;
  config
    .set_merge_policy(MergeOnXMergePolicy::new(
      new_merge_policy(&mut random)?,
      if use_get_reader {
        MergeTrigger::GetReader
      } else {
        MergeTrigger::Commit
      },
    ))
    .set_max_full_flush_merge_wait_millis(30 * 1000)
    .set_merge_scheduler(MergeSchedulerEnum::LatchedSerial(
      LatchedSerialMergeScheduler::new(wait_for_merge.clone(), wait_for_delete_all.clone()),
    ));
  let writer = IndexWriter::new(directory, config)?;

  writer.add_document(id_doc("1")?)?;
  writer.flush()?;
  writer.add_document(id_doc("2")?)?;

  let thread_writer = writer.clone();
  let thread_wait_for_merge = wait_for_merge.clone();
  let handle = thread::spawn(move || -> Result<()> {
    let result = if use_get_reader {
      directory_reader::open_from_writer(&thread_writer).and_then(|reader| reader.close())
    } else {
      thread_writer.commit().map(|_| ())
    };
    if result.is_err() {
      thread_wait_for_merge.count_down();
    }
    result
  });

  await_latch(&wait_for_merge, "merge did not start")?;
  writer.delete_all()?;
  wait_for_delete_all.count_down();
  join_result(handle)?;

  writer.close()?;
  Ok(())
}

#[test]
fn test_abort_merge_on_commit() -> Result<()> {
  abort_merge_on_x(false)
}

#[test]
fn test_abort_merge_on_get_reader() -> Result<()> {
  abort_merge_on_x(true)
}

#[test]
fn test_force_merge_while_get_reader() -> Result<()> {
  let mut random = random();
  let directory = new_directory_shared(&mut random)?;
  let wait_for_merge = TestLatch::new();
  let wait_for_force_merge_called = TestLatch::new();
  let mut config = new_index_writer_config(&mut random)?;
  config
    .set_merge_policy(MergeOnXMergePolicy::new(
      new_merge_policy(&mut random)?,
      MergeTrigger::GetReader,
    ))
    .set_max_full_flush_merge_wait_millis(30 * 1000)
    .set_merge_scheduler(MergeSchedulerEnum::LatchedSerial(
      LatchedSerialMergeScheduler::new(wait_for_merge.clone(), wait_for_force_merge_called.clone()),
    ));
  let writer = IndexWriter::new(directory, config)?;

  writer.add_document(id_doc("1")?)?;
  writer.flush()?;
  writer.add_document(id_doc("2")?)?;

  let thread_writer = writer.clone();
  let handle = thread::spawn(move || -> Result<()> {
    let reader = directory_reader::open_from_writer(&thread_writer)?;
    assert_eq!(2, reader.max_doc()?);
    reader.close()
  });

  await_latch(&wait_for_merge, "merge did not start")?;
  writer.add_document(id_doc("3")?)?;
  wait_for_force_merge_called.count_down();
  writer.force_merge(1)?;
  join_result(handle)?;

  writer.close()?;
  Ok(())
}

#[test]
fn test_fail_after_merge_committed() -> Result<()> {
  struct FailAfterFlushHooks<D>
  where
    D: Directory,
  {
    merge_and_fail: Arc<AtomicBool>,
    writer: Arc<OnceLock<Arc<IndexWriter<D>>>>,
  }

  impl<D> IndexWriterHooks for FailAfterFlushHooks<D>
  where
    D: Directory + 'static,
  {
    fn do_after_flush(&self) -> Result<()> {
      if self.merge_and_fail.load(Ordering::SeqCst)
        && let Some(writer) = self.writer.get()
        && writer.has_pending_merges()?
      {
        writer.execute_merge(MergeTrigger::GetReader)?;
        return Err(LuceneError::illegal_state("boom"));
      }
      Ok(())
    }
  }

  let mut random = random();
  let directory = new_directory_shared(&mut random)?;
  let merge_and_fail = Arc::new(AtomicBool::new(false));
  let writer_cell = Arc::new(OnceLock::new());
  let mut config = new_index_writer_config(&mut random)?;
  config
    .set_merge_policy(MergeOnXMergePolicy::new(
      NoMergePolicy::default().into(),
      MergeTrigger::GetReader,
    ))
    .set_max_full_flush_merge_wait_millis(30 * 1000)
    .set_merge_scheduler(SerialMergeScheduler::new());
  let writer = IndexWriter::with_hooks(
    directory,
    config,
    Some(IndexWriterHooksEnum::custom(FailAfterFlushHooks {
      merge_and_fail: merge_and_fail.clone(),
      writer: writer_cell.clone(),
    })),
  )?;
  let _ = writer_cell.set(writer.clone());

  writer.add_document(id_doc("1")?)?;
  writer.flush()?;
  writer.add_document(id_doc("2")?)?;
  writer.flush()?;

  merge_and_fail.store(true, Ordering::SeqCst);
  let result = directory_reader::open_from_writer(&writer);
  merge_and_fail.store(false, Ordering::SeqCst);
  match result {
    Ok(reader) => {
      reader.close()?;
      return Err(LuceneError::illegal_state("expected boom"));
    },
    Err(err) => assert_eq!("boom", err.to_string()),
  }

  writer.close()?;
  Ok(())
}

#[test]
fn test_stress_update_same_document_with_merge_on_get_reader() -> Result<()> {
  stress_update_same_document_with_merge_on_x(true)
}

#[test]
fn test_stress_update_same_document_with_merge_on_commit() -> Result<()> {
  stress_update_same_document_with_merge_on_x(false)
}

fn stress_update_same_document_with_merge_on_x(use_get_reader: bool) -> Result<()> {
  let mut random = random();
  let directory = new_directory_shared(&mut random)?;
  let mut config = new_index_writer_config(&mut random)?;
  config
    .set_merge_policy(MergeOnXMergePolicy::new(
      new_merge_policy(&mut random)?,
      if use_get_reader {
        MergeTrigger::GetReader
      } else {
        MergeTrigger::Commit
      },
    ))
    .set_max_full_flush_merge_wait_millis(10 + random.random_range(0..2000))
    .set_soft_deletes_field("soft_delete")
    .set_merge_scheduler(ConcurrentMergeScheduler::new());
  let writer = IndexWriter::new(directory.clone(), config)?;

  writer.soft_update_document(
    Term::from_text("id", "1"),
    id_doc("1")?,
    soft_delete_marker(),
  )?;
  writer.commit()?;

  let iters = Arc::new(AtomicI32::new(20 + random.random_range(0..10)));
  let num_full_flushes = Arc::new(AtomicI32::new(5 + random.random_range(0..5)));
  let done = Arc::new(AtomicBool::new(false));
  let mut threads = Vec::new();
  for _ in 0..1 {
    let thread_writer = writer.clone();
    let thread_iters = iters.clone();
    let thread_num_full_flushes = num_full_flushes.clone();
    let thread_done = done.clone();
    threads.push(thread::spawn(move || -> Result<()> {
      while thread_iters.fetch_sub(1, Ordering::SeqCst) > 0
        || thread_num_full_flushes.load(Ordering::SeqCst) > 0
      {
        thread_writer.soft_update_document(
          Term::from_text("id", "1"),
          id_doc("1")?,
          soft_delete_marker(),
        )?;
        if thread_iters.load(Ordering::SeqCst) & 1 == 0 {
          thread_writer.add_document(Document::new())?;
        }
      }
      thread_done.store(true, Ordering::SeqCst);
      Ok(())
    }));
  }

  while !done.load(Ordering::SeqCst) {
    if use_get_reader {
      let reader = directory_reader::open_from_writer(&writer)?;
      let searcher = index_searcher::from_reader(reader)?;
      assert_eq!(
        1,
        searcher.count(TermQuery::new(Term::from_text("id", "1")))?
      );
    } else {
      if num_full_flushes.load(Ordering::SeqCst) & 1 == 0 {
        writer.commit()?;
      }
      let delegate = directory_reader::open(directory.clone())?;
      let open = SoftDeletesDirectoryReaderWrapper::new(delegate, "soft_delete")?;
      let searcher = index_searcher::from_reader(open)?;
      assert_eq!(
        1,
        searcher.count(TermQuery::new(Term::from_text("id", "1")))?
      );
    }
    num_full_flushes.fetch_sub(1, Ordering::SeqCst);
  }
  num_full_flushes.store(0, Ordering::SeqCst);
  for thread in threads {
    join_result(thread)?;
  }

  writer.close()?;
  Ok(())
}

#[test]
fn test_merge_on_get_reader() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config.set_merge_policy(NoMergePolicy::default());
  let first_writer = IndexWriter::new(dir.clone(), config)?;
  let mut field_types = HashMap::new();
  for _ in 0..5 {
    crate::test_framework::core::index::test_index_writer::add_doc(
      &mut random,
      &first_writer,
      &mut field_types,
    )?;
    first_writer.flush()?;
  }
  {
    let first_reader = directory_reader::open_from_writer(&first_writer)?;
    let first_ctx = first_reader.get_context()?;
    assert_eq!(5, first_ctx.leaves()?.len());
  }
  first_writer.close()?;

  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config
    .set_merge_policy(MergeOnXMergePolicy::new(
      new_merge_policy(&mut random)?,
      MergeTrigger::GetReader,
    ))
    .set_max_full_flush_merge_wait_millis(i64::MAX);
  let writer_with_merge_policy = IndexWriter::new(dir.clone(), config)?;

  {
    let unmerged_reader = directory_reader::open(dir.clone())?;
    let unmerged_ctx = unmerged_reader.get_context()?;
    assert_eq!(5, unmerged_ctx.leaves()?.len());
  }

  crate::test_framework::core::index::test_index_writer::add_doc(
    &mut random,
    &writer_with_merge_policy,
    &mut field_types,
  )?;
  {
    let merged_reader = directory_reader::open_from_writer(&writer_with_merge_policy)?;
    let merged_ctx = merged_reader.get_context()?;
    assert_eq!(1, merged_ctx.leaves()?.len());
  }

  writer_with_merge_policy.close()?;
  Ok(())
}

#[test]
fn test_set_diagnostics() -> Result<()> {
  let mut random = random();
  let mut log_mp = new_log_merge_policy_with_merge_factor(&mut random, 4)?;
  match &mut log_mp {
    MergePolicyEnum::LogDoc(log_mp) => log_mp.set_target_search_concurrency(1)?,
    MergePolicyEnum::LogBytesSize(log_mp) => log_mp.set_target_search_concurrency(1)?,
    _ => unreachable!("expected a LogMergePolicy"),
  }
  let my_merge_policy = SetDiagnosticsMergePolicy::new(log_mp);
  let dir = new_directory_shared(&mut random)?;
  let mut config = new_index_writer_config(&mut random)?;
  config
    .set_merge_policy(my_merge_policy)
    .set_max_buffered_docs(2);
  let w = IndexWriter::new(dir.clone(), config)?;
  let doc = Document::new();
  for _ in 0..20 {
    w.add_document(doc.clone())?;
  }
  w.close()?;
  let si = SegmentInfos::read_latest_commit(dir.clone())?;
  let mut has_one_merged_segment = false;
  for sci in si.iter() {
    if sci.info.get_diagnostics().get(SOURCE).map(String::as_str) == Some(SOURCE_MERGE) {
      assert_eq!(
        Some("my_merge_policy"),
        sci
          .info
          .get_diagnostics()
          .get("merge_policy")
          .map(String::as_str)
      );
      has_one_merged_segment = true;
    }
  }
  assert!(has_one_merged_segment);
  w.close()?;
  dir.close()?;
  Ok(())
}

#[test]
fn test_force_merge_dv_update_file_with_concurrent_flush() -> Result<()> {
  let mut random = random();
  let wait_for_init_merge_reader = CountDownLatch::new(1);
  let wait_for_dv_update = CountDownLatch::new(1);
  let wait_for_merge_finished = CountDownLatch::new(1);

  let temp_dir = create_temp_dir_with_prefix("testForceMergeDVUpdateFileWithConcurrentFlush")?;
  let path = temp_dir.path().to_path_buf();
  let fs_directory = new_fs_directory(&mut random, temp_dir)?;
  let mock_directory = Arc::new(MockAssertFileExistDirectory::new(fs_directory, path));

  let mock_merge_policy = OneMergeWrappingMergePolicy::new(
    SoftDeletesRetentionMergePolicy::new(
      "soft_delete",
      || Ok(MatchAllDocsQuery::new().into()),
      ForceMergeDvUpdateMergePolicy::new(),
    ),
    ForceMergeDvUpdateOneMergeUnaryOperator::new(
      wait_for_init_merge_reader.clone(),
      wait_for_dv_update.clone(),
    ),
  );
  let mut config = new_index_writer_config(&mut random)?;
  config
    .set_merge_policy(mock_merge_policy)
    .set_soft_deletes_field("soft_delete");
  let writer = IndexWriter::new(mock_directory.clone(), config)?;

  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "1", Store::Yes)?);
  doc.add(StringField::from_string("version", "1", Store::Yes)?);
  writer.add_document(doc)?;
  writer.flush()?;
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "2", Store::Yes)?);
  doc.add(StringField::from_string("version", "1", Store::Yes)?);
  writer.add_document(doc)?;
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "2", Store::Yes)?);
  doc.add(StringField::from_string("version", "2", Store::Yes)?);
  let field = NumericDocValuesField::new("soft_delete", 1);
  writer.soft_update_document(Term::from_text("id", "2"), doc, vec![field.into()])?;
  writer.flush()?;

  let force_merge_writer = writer.clone();
  let merge_finished_latch = wait_for_merge_finished.clone();
  let force_merge_thread = thread::spawn(move || -> Result<()> {
    let force_merge_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      force_merge_writer.force_merge(1)
    }));
    merge_finished_latch.count_down();
    match force_merge_result {
      Ok(result) => result,
      Err(payload) => Err(LuceneError::tragedy_from_panic(
        "panic while force merging",
        payload.as_ref(),
      )),
    }
  });
  wait_for_init_merge_reader.wait();

  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "2", Store::Yes)?);
  doc.add(StringField::from_string("version", "3", Store::Yes)?);
  let field = NumericDocValuesField::new("soft_delete", 1);
  writer.soft_update_document(Term::from_text("id", "2"), doc, vec![field.into()])?;
  writer.flush()?;

  wait_for_dv_update.count_down();
  wait_for_merge_finished.wait();
  match force_merge_thread.join() {
    Ok(result) => result?,
    Err(payload) => {
      return Err(LuceneError::tragedy_from_panic(
        "panic while force merging",
        payload.as_ref(),
      ));
    },
  }

  writer.close()?;
  mock_directory.as_ref().close()?;
  Ok(())
}

#[test]
fn test_merge_dv_update_file_on_get_reader_with_concurrent_flush() -> Result<()> {
  let mut random = random();
  let wait_for_init_merge_reader = CountDownLatch::new(1);
  let wait_for_dv_update = CountDownLatch::new(1);

  let temp_dir =
    create_temp_dir_with_prefix("testMergeDVUpdateFileOnGetReaderWithConcurrentFlush")?;
  let path = temp_dir.path().to_path_buf();
  let fs_directory = new_fs_directory(&mut random, temp_dir)?;
  let mock_directory = Arc::new(MockAssertFileExistDirectory::new(fs_directory, path));

  let analyzer = MockAnalyzer::new(&mut random);
  let mut first_config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  first_config.set_merge_policy(NoMergePolicy::default());
  let first_writer = IndexWriter::new(mock_directory.clone(), first_config)?;

  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "1", Store::Yes)?);
  doc.add(StringField::from_string("version", "1", Store::Yes)?);
  first_writer.add_document(doc)?;
  first_writer.flush()?;
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "2", Store::Yes)?);
  doc.add(StringField::from_string("version", "1", Store::Yes)?);
  first_writer.add_document(doc)?;
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "2", Store::Yes)?);
  doc.add(StringField::from_string("version", "2", Store::Yes)?);
  let field = NumericDocValuesField::new("soft_delete", 1);
  first_writer.soft_update_document(Term::from_text("id", "2"), doc, vec![field.into()])?;
  first_writer.flush()?;
  let first_reader = directory_reader::open_from_writer(&first_writer)?;
  let first_context = first_reader.get_context()?;
  assert_eq!(2, first_context.leaves()?.len());
  first_context.reader().close()?;
  first_writer.close()?;

  let mock_concurrent_merge_scheduler = ConcurrentMergeScheduler::with_hook(
    ConcurrentMergeSchedulerHook::MergeDvUpdateFileOnGetReader(
      MergeDvUpdateFileOnGetReaderConcurrentMergeScheduler::new(
        wait_for_init_merge_reader.clone(),
        wait_for_dv_update.clone(),
      ),
    ),
  );

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  iwc
    .set_merge_policy(MergeOnXMergePolicy::new(
      new_merge_policy(&mut random)?,
      MergeTrigger::GetReader,
    ))
    .set_max_full_flush_merge_wait_millis(i64::MAX)
    .set_merge_scheduler(mock_concurrent_merge_scheduler);
  let writer_with_merge_policy = IndexWriter::new(mock_directory.clone(), iwc)?;

  let thread_writer = writer_with_merge_policy.clone();
  let update_thread = thread::spawn(move || -> Result<()> {
    wait_for_init_merge_reader.wait();

    let mut update_doc = Document::new();
    update_doc.add(StringField::from_string("id", "2", Store::Yes)?);
    update_doc.add(StringField::from_string("version", "3", Store::Yes)?);
    let soft_delete_field = NumericDocValuesField::new("soft_delete", 1);
    thread_writer.soft_update_document(
      Term::from_text("id", "2"),
      update_doc,
      vec![soft_delete_field.into()],
    )?;
    let reader = directory_reader::open_with_writer_deletes(&thread_writer, true, false)?;
    reader.close()?;

    wait_for_dv_update.count_down();
    Ok(())
  });

  let merged_reader = directory_reader::open_from_writer(&writer_with_merge_policy)?;
  let merged_context = merged_reader.get_context()?;
  assert_eq!(1, merged_context.leaves()?.len());
  merged_context.reader().close()?;
  match update_thread.join() {
    Ok(result) => result?,
    Err(payload) => {
      return Err(LuceneError::tragedy_from_panic(
        "panic while updating doc values",
        payload.as_ref(),
      ));
    },
  }

  writer_with_merge_policy.close()?;
  mock_directory.as_ref().close()?;
  Ok(())
}

#[test]
fn test_merge_dv_update_file_on_commit_with_concurrent_flush() -> Result<()> {
  let mut random = random();
  let wait_for_init_merge_reader = CountDownLatch::new(1);
  let wait_for_dv_update = CountDownLatch::new(1);

  let temp_dir = create_temp_dir_with_prefix("testMergeDVUpdateFileOnCommitWithConcurrentFlush")?;
  let path = temp_dir.path().to_path_buf();
  let fs_directory = new_fs_directory(&mut random, temp_dir)?;
  let mock_directory = Arc::new(MockAssertFileExistDirectory::new(fs_directory, path));

  let analyzer = MockAnalyzer::new(&mut random);
  let mut first_config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  first_config.set_merge_policy(NoMergePolicy::default());
  let first_writer = IndexWriter::new(mock_directory.clone(), first_config)?;

  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "1", Store::Yes)?);
  doc.add(StringField::from_string("version", "1", Store::Yes)?);
  first_writer.add_document(doc)?;
  first_writer.flush()?;
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "2", Store::Yes)?);
  doc.add(StringField::from_string("version", "1", Store::Yes)?);
  first_writer.add_document(doc)?;
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "2", Store::Yes)?);
  doc.add(StringField::from_string("version", "2", Store::Yes)?);
  let field = NumericDocValuesField::new("soft_delete", 1);
  first_writer.soft_update_document(Term::from_text("id", "2"), doc, vec![field.into()])?;
  first_writer.flush()?;
  let first_reader = directory_reader::open_from_writer(&first_writer)?;
  let first_context = first_reader.get_context()?;
  assert_eq!(2, first_context.leaves()?.len());
  first_context.reader().close()?;
  first_writer.close()?;

  let mock_concurrent_merge_scheduler =
    ConcurrentMergeScheduler::with_hook(ConcurrentMergeSchedulerHook::MergeDvUpdateFileOnCommit(
      MergeDvUpdateFileOnCommitConcurrentMergeScheduler::new(
        wait_for_init_merge_reader.clone(),
        wait_for_dv_update.clone(),
      ),
    ));

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  iwc
    .set_merge_policy(MergeOnXMergePolicy::new(
      new_merge_policy(&mut random)?,
      MergeTrigger::Commit,
    ))
    .set_max_full_flush_merge_wait_millis(i64::MAX)
    .set_merge_scheduler(mock_concurrent_merge_scheduler);
  let writer_with_merge_policy = IndexWriter::new(mock_directory.clone(), iwc)?;

  let thread_writer = writer_with_merge_policy.clone();
  let update_thread = thread::spawn(move || -> Result<()> {
    wait_for_init_merge_reader.wait();

    let mut update_doc = Document::new();
    update_doc.add(StringField::from_string("id", "2", Store::Yes)?);
    update_doc.add(StringField::from_string("version", "3", Store::Yes)?);
    let soft_delete_field = NumericDocValuesField::new("soft_delete", 1);
    thread_writer.soft_update_document(
      Term::from_text("id", "2"),
      update_doc,
      vec![soft_delete_field.into()],
    )?;
    let reader = directory_reader::open_with_writer_deletes(&thread_writer, true, false)?;
    reader.close()?;

    wait_for_dv_update.count_down();
    Ok(())
  });

  writer_with_merge_policy.commit()?;
  assert_eq!(2, writer_with_merge_policy.get_segment_count());
  match update_thread.join() {
    Ok(result) => result?,
    Err(payload) => {
      return Err(LuceneError::tragedy_from_panic(
        "panic while updating doc values",
        payload.as_ref(),
      ));
    },
  }

  writer_with_merge_policy.close()?;
  mock_directory.as_ref().close()?;
  Ok(())
}

#[test]
fn test_force_merge_with_pending_hard_and_soft_delete_file() -> Result<()> {
  let mut random = random();
  let temp_dir = create_temp_dir_with_prefix("testForceMergeWithPendingHardAndSoftDeleteFile")?;
  let path = temp_dir.path().to_path_buf();
  let fs_directory = new_fs_directory(&mut random, temp_dir)?;
  let mock_directory = Arc::new(MockAssertFileExistDirectory::new(fs_directory, path));

  let mock_merge_policy = OneMergeWrappingMergePolicy::new(
    OnlyForceMergeMergePolicy::new(TieredMergePolicy::new()),
    NewOneMergeUnaryOperator,
  );
  let mut config = new_index_writer_config(&mut random)?;
  config.set_merge_policy(mock_merge_policy);

  let writer = IndexWriter::new(mock_directory, config)?;
  let mut field_types = HashMap::new();

  let mut doc = Document::new();
  doc.add(new_string_field(
    &mut random,
    "id",
    "1",
    Store::Yes,
    &mut field_types,
  )?);
  doc.add(new_string_field(
    &mut random,
    "version",
    "1",
    Store::Yes,
    &mut field_types,
  )?);
  writer.add_document(doc)?;
  writer.commit()?;

  let mut doc = Document::new();
  doc.add(new_string_field(
    &mut random,
    "id",
    "2",
    Store::Yes,
    &mut field_types,
  )?);
  doc.add(new_string_field(
    &mut random,
    "version",
    "1",
    Store::Yes,
    &mut field_types,
  )?);
  writer.add_document(doc)?;

  let mut doc = Document::new();
  doc.add(new_string_field(
    &mut random,
    "id",
    "3",
    Store::Yes,
    &mut field_types,
  )?);
  doc.add(new_string_field(
    &mut random,
    "version",
    "1",
    Store::Yes,
    &mut field_types,
  )?);
  writer.add_document(doc)?;

  let mut doc = Document::new();
  doc.add(new_string_field(
    &mut random,
    "id",
    "4",
    Store::Yes,
    &mut field_types,
  )?);
  doc.add(new_string_field(
    &mut random,
    "version",
    "1",
    Store::Yes,
    &mut field_types,
  )?);
  writer.add_document(doc)?;

  let mut doc = Document::new();
  doc.add(new_string_field(
    &mut random,
    "id",
    "5",
    Store::Yes,
    &mut field_types,
  )?);
  doc.add(new_string_field(
    &mut random,
    "version",
    "1",
    Store::Yes,
    &mut field_types,
  )?);
  writer.add_document(doc)?;
  writer.commit()?;

  let mut doc = Document::new();
  doc.add(new_string_field(
    &mut random,
    "id",
    "2",
    Store::Yes,
    &mut field_types,
  )?);
  doc.add(new_string_field(
    &mut random,
    "version",
    "2",
    Store::Yes,
    &mut field_types,
  )?);
  writer.update_document_with_term(Term::from_text("id", "2"), doc)?;
  writer.commit()?;

  let mut doc = Document::new();
  doc.add(new_string_field(
    &mut random,
    "id",
    "3",
    Store::Yes,
    &mut field_types,
  )?);
  doc.add(new_string_field(
    &mut random,
    "version",
    "2",
    Store::Yes,
    &mut field_types,
  )?);
  writer.update_document_with_term(Term::from_text("id", "3"), doc)?;

  let mut doc = Document::new();
  doc.add(new_string_field(
    &mut random,
    "id",
    "4",
    Store::Yes,
    &mut field_types,
  )?);
  doc.add(new_string_field(
    &mut random,
    "version",
    "2",
    Store::Yes,
    &mut field_types,
  )?);
  let field = NumericDocValuesField::new("soft_delete", 1);
  writer.soft_update_document(Term::from_text("id", "4"), doc, vec![field.into()])?;

  let reader = writer.get_reader(true, false)?;
  reader.close()?;
  writer.commit()?;

  writer.force_merge(1)?;
  writer.close()?;
  Ok(())
}

struct MockAssertFileExistDirectory<D> {
  in_: D,
  path: PathBuf,
  id: Identity,
}

impl<D> MockAssertFileExistDirectory<D>
where
  D: Directory,
{
  fn new(in_: D, path: PathBuf) -> Self {
    Self {
      in_,
      path,
      id: Identity::new(),
    }
  }
}

impl<D> Display for MockAssertFileExistDirectory<D>
where
  D: Directory,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "MockAssertFileExistDirectory({})", self.in_)
  }
}

impl<D> CloseableRef for MockAssertFileExistDirectory<D>
where
  D: Directory,
{
  fn close(&self) -> Result<()> {
    self.in_.close()
  }
}

impl<D> HasIdentity for MockAssertFileExistDirectory<D>
where
  D: Directory,
{
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl<D> Directory for MockAssertFileExistDirectory<D>
where
  D: Directory,
  D::IndexInput: IndexInput<IndexInput = D::IndexInput>,
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

  type IndexInput = MockAssertFileExistIndexInput<D::IndexInput>;

  fn open_input(&self, name: &str, context: &IOContext) -> Result<Self::IndexInput> {
    let index_input = self.in_.open_input(name, context)?;
    Ok(MockAssertFileExistIndexInput::new(
      name.to_string(),
      index_input,
      self.path.join(name),
    ))
  }

  type Lock = D::Lock;

  fn obtain_lock(&self, name: &str) -> Result<Self::Lock> {
    self.in_.obtain_lock(name)
  }

  fn copy_from<F>(&self, from: &F, src: &str, dest: &str, context: &IOContext) -> Result<()>
  where
    F: Directory + ?Sized,
  {
    self.in_.copy_from(from, src, dest, context)
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

struct MockAssertFileExistIndexInput<I> {
  name: String,
  delegate: I,
  file_path: PathBuf,
}

impl<I> MockAssertFileExistIndexInput<I>
where
  I: IndexInput,
{
  fn new(name: String, in_: I, file_path: PathBuf) -> Self {
    Self {
      name,
      delegate: in_,
      file_path,
    }
  }

  fn check_file_exists(&self) -> Result<()> {
    if !self.file_path.exists() {
      return Err(LuceneError::io_with_path(
        self.file_path.to_string_lossy().to_string(),
        Error::new(
          ErrorKind::NotFound,
          self.file_path.to_string_lossy().to_string(),
        ),
      ));
    }
    Ok(())
  }
}

impl<I> CloseableRef for MockAssertFileExistIndexInput<I>
where
  I: IndexInput,
{
  fn close(&self) -> Result<()> {
    self.delegate.close()
  }
}

impl<I> DataInput for MockAssertFileExistIndexInput<I>
where
  I: IndexInput,
{
  fn read_byte(&mut self) -> Result<u8> {
    self.check_file_exists()?;
    self.delegate.read_byte()
  }

  fn read_bytes(&mut self, b: &mut [u8], offset: usize, len: usize) -> Result<()> {
    self.check_file_exists()?;
    self.delegate.read_bytes(b, offset, len)
  }

  fn read_group_vint(&mut self, dst: &mut [i32], offset: usize) -> Result<()> {
    self.check_file_exists()?;
    self.delegate.read_group_vint(dst, offset)
  }

  fn skip_bytes(&mut self, num_bytes: i64) -> Result<()> {
    self.check_file_exists()?;
    IndexInput::skip_bytes(&mut self.delegate, num_bytes)
  }
}

impl<I> Display for MockAssertFileExistIndexInput<I>
where
  I: IndexInput,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(
      f,
      "MockAssertFileExistIndexInput(name={} delegate={})",
      self.name, self.delegate
    )
  }
}

impl<I> TryClone for MockAssertFileExistIndexInput<I>
where
  I: IndexInput,
{
  fn try_clone(&self) -> Result<Self>
  where
    Self: Sized,
  {
    Ok(Self::new(
      self.name.clone(),
      self.delegate.try_clone()?,
      self.file_path.clone(),
    ))
  }
}

impl<I> IndexInput for MockAssertFileExistIndexInput<I>
where
  I: IndexInput<IndexInput = I>,
{
  type IndexInput = MockAssertFileExistIndexInput<I>;

  fn get_file_pointer(&self) -> Result<usize> {
    self.delegate.get_file_pointer()
  }

  fn seek(&mut self, pos: usize) -> Result<()> {
    self.check_file_exists()?;
    self.delegate.seek(pos)
  }

  fn length(&self) -> Result<usize> {
    self.delegate.length()
  }

  fn slice(
    &self,
    slice_description: &str,
    offset: usize,
    length: usize,
  ) -> Result<Self::IndexInput> {
    self.check_file_exists()?;
    let slice = self.delegate.slice(slice_description, offset, length)?;
    Ok(Self::new(
      slice_description.to_string(),
      slice,
      self.file_path.clone(),
    ))
  }

  type RandomAccessSlice = RandomAccessInputWrapper<MockAssertFileExistIndexInput<I>>;

  fn random_access_slice(&self, offset: usize, length: usize) -> Result<Self::RandomAccessSlice> {
    Ok(RandomAccessInputWrapper::new(self.slice(
      "randomaccess",
      offset,
      length,
    )?))
  }
}
