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
use crate::test_framework::core::util::lucene_test_case::{
  at_least, new_directory_shared, new_index_writer_config, new_index_writer_config_with_analyzer,
  new_io_context, new_log_merge_policy_with_merge_factor_cfs, new_mock_directory, new_string_field,
  new_text_field, random, slow_file_exists,
};
use rand::{Rng, RngExt};
use std::collections::HashMap;

use crate::core::index::concurrent_merge_scheduler::{
  ConcurrentMergeScheduler, ConcurrentMergeSchedulerHook,
};
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::{IndexWriterConfig, OpenMode};
use crate::core::index::keep_only_last_commit_deletion_policy::KeepOnlyLastCommitDeletionPolicy;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::merge_policy::MergePolicy;
use crate::core::index::merge_scheduler::MergeSchedulerEnum;
use crate::core::index::no_merge_policy::NoMergePolicy;
use crate::core::index::segment_infos::SegmentInfos;
use crate::core::index::serial_merge_scheduler::SerialMergeScheduler;
use crate::core::index::snapshot_deletion_policy::SnapshotDeletionPolicy;
use crate::core::index::term::Term;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::index::{CODEC_FILE_PATTERN, IndexFileNames};
use crate::core::store::directory::Directory;
use crate::core::store::{DataInput, DataOutput, IndexInput};
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, LuceneErrorKind, Result};
use crate::core::util::info_stream::{InfoStreamMT, get_default_info_stream};
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::index::test_index_file_deleter::FakeFailConcurrentMergeScheduler;
use crate::test_framework::core::store::mock_directory_wrapper::{
  Failure, FakeIOException, MockDirectoryWrapper, Throttling,
};
use crate::test_framework::core::util::failure_context::{
  ExecutionMethod, ExecutionOwner, FailureContext, FailurePoint,
};
#[allow(dead_code)] // for quick search
struct TestIndexFileDeleter;

use crate::core::index::index_file_deleter::{IndexFileDeleter, inflate_gens};
use std::collections::HashSet;
use std::io::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[test]
fn test_delete_left_over_files() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let mut merge_policy = new_log_merge_policy_with_merge_factor_cfs(&mut random, true, 10)?;

  // This test expects all of its segments to be in CFS
  merge_policy.get_base_mut().set_no_cfs_ratio(1.0)?;
  merge_policy
    .get_base_mut()
    .set_max_cfs_segment_size_mb(f64::INFINITY)?;

  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config
    .set_max_buffered_docs(10)
    .set_merge_policy(merge_policy)
    .set_use_compound_file(true);
  let writer = IndexWriter::new(dir.clone(), config)?;

  let mut field_types = HashMap::new();
  let mut i = 0;
  while i < 35 {
    add_doc(&mut random, &writer, i, &mut field_types)?;
    i += 1;
  }
  writer
    .get_config()?
    .get_merge_policy_mut()
    .get_base_mut()
    .set_no_cfs_ratio(0.0)?;
  writer.get_config()?.set_use_compound_file(false);
  while i < 45 {
    add_doc(&mut random, &writer, i, &mut field_types)?;
    i += 1;
  }
  writer.close()?;
  drop(writer);

  // Delete one doc so we get a .del file:
  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config
    .set_merge_policy(NoMergePolicy::default())
    .set_use_compound_file(true);
  let writer = IndexWriter::new(dir.clone(), config)?;
  let search_term = Term::from_text("id", "7");
  writer.delete_documents_with_terms(vec![search_term])?;
  writer.close()?;
  drop(writer);

  // read in index to try to not depend on codec-specific filenames so much
  let sis = SegmentInfos::read_latest_commit(dir.clone())?;
  let _si0 = &sis.info(0).unwrap().info;
  let _si1 = &sis.info(1).unwrap().info;
  let _si3 = &sis.info(3).unwrap().info;

  // Now, artificially create an extra .del file & extra
  // .s0 file:
  let mut files = dir.list_all()?;

  let ext = ".liv";

  // Create a bogus separate del file for a
  // segment that already has a separate del file:
  copy_file(
    &mut random,
    dir.as_ref(),
    &format!("_0_1{}", ext),
    &format!("_0_2{}", ext),
  )?;

  // Create a bogus separate del file for a
  // segment that does not yet have a separate del file:
  copy_file(
    &mut random,
    dir.as_ref(),
    &format!("_0_1{}", ext),
    &format!("_1_1{}", ext),
  )?;

  // Create a bogus separate del file for a
  // non-existent segment:
  copy_file(
    &mut random,
    dir.as_ref(),
    &format!("_0_1{}", ext),
    &format!("_188_1{}", ext),
  )?;

  // TODO: SimpleTextCodec is not implemented.
  let cfs_files0 = ["_0.cfs", "_0.cfe"];

  // Create a bogus segment file:
  copy_file(&mut random, dir.as_ref(), cfs_files0[0], "_188.cfs")?;

  // Create a bogus fnm file when the CFS already exists:
  copy_file(&mut random, dir.as_ref(), cfs_files0[0], "_0.fnm")?;

  // Create a bogus cfs file shadowing a non-cfs segment:

  assert!(slow_file_exists(dir.as_ref(), "_3.fdt")? || slow_file_exists(dir.as_ref(), "_3.fld")?);

  // TODO: SimpleTextCodec is not implemented.
  let cfs_files3 = ["_3.cfs", "_3.cfe"];
  for f in cfs_files3 {
    assert!(!slow_file_exists(dir.as_ref(), f)?);
  }

  // TODO: SimpleTextCodec is not implemented.
  let cfs_files1 = ["_1.cfs", "_1.cfe"];
  copy_file(&mut random, dir.as_ref(), cfs_files1[0], "_3.cfs")?;

  let files_pre = dir.list_all()?;

  // Open & close a writer: it should delete the above files and nothing more:
  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config.set_open_mode(OpenMode::Append);
  let writer = IndexWriter::new(dir.clone(), config)?;
  writer.close()?;
  drop(writer);

  let mut files2 = dir.list_all()?;

  files.sort();
  files2.sort();

  let dif = diff_files(&files, &files2);

  if files != files2 {
    panic!(
      "IndexFileDeleter failed to delete unreferenced extra files: should have deleted {} files but only deleted {}; expected files:\n    {}\n  actual files:\n    {}\ndiff: {:?}",
      files_pre.len() - files.len(),
      files_pre.len() - files2.len(),
      to_string(&files),
      to_string(&files2),
      dif
    );
  }

  Ok(())
}

fn diff_files(files1: &[String], files2: &[String]) -> HashSet<String> {
  let set1: HashSet<&String> = files1.iter().collect();
  let set2: HashSet<&String> = files2.iter().collect();
  let mut extra = HashSet::new();

  for item in &set1 {
    if !set2.contains(item) {
      extra.insert((*item).clone());
    }
  }
  for item in &set2 {
    if !set1.contains(item) {
      extra.insert((*item).clone());
    }
  }

  extra
}

fn to_string(list: &[String]) -> String {
  let mut s = String::new();
  for (i, item) in list.iter().enumerate() {
    if i > 0 {
      s.push_str("\n    ");
    }
    s.push_str(item);
  }
  s
}
fn copy_file<D, R>(random: &mut R, dir: &D, src: &str, dest: &str) -> Result<()>
where
  D: Directory,
  R: Rng + ?Sized,
{
  let mut input = dir.open_input(src, &new_io_context(random)?)?;
  let mut output = dir.create_output(dest, &new_io_context(random)?)?;
  let mut buffer = [0u8; 1024];
  let mut remainder = input.length()?;

  while remainder > 0 {
    let len = usize::min(buffer.len(), remainder);
    input.read_bytes(&mut buffer, 0, len)?;
    output.write_bytes_with_len(&buffer, len)?;
    remainder -= len;
  }
  output.close()?;

  Ok(())
}
fn add_doc<D, R>(
  random: &mut R,
  writer: &IndexWriter<D>,
  id: i32,
  field_types: &mut HashMap<String, FieldType>,
) -> Result<()>
where
  R: Rng + ?Sized,
  D: Directory + 'static,
{
  let mut doc = Document::new();

  doc.add(new_text_field(
    random,
    "content",
    "aaa",
    Store::No,
    field_types,
  )?);

  doc.add(new_string_field(
    random,
    "id",
    id.to_string(),
    Store::No,
    field_types,
  )?);
  writer.add_document(doc)?;
  Ok(())
}
#[test]
fn test_virus_scanner_doesnt_corrupt_index() -> Result<()> {
  // TODO addVirusChecker is not implemented.
  test_not_required_in_rust_lucene!();
}

#[test]
fn test_no_segments_dot_gen_inflation() -> Result<()> {
  let mut random = random();
  let dir = new_mock_directory(&mut random)?;

  // empty commit
  let writer = IndexWriter::new(Arc::new(dir.clone()), IndexWriterConfig::new()?)?;
  writer.close()?;
  drop(writer);

  let mut sis = SegmentInfos::read_latest_commit(Arc::new(dir.clone()))?;
  assert_eq!(1, sis.get_generation());

  // no inflation
  inflate_gens_test(&mut sis, dir.list_all()?, &get_default_info_stream())?;
  assert_eq!(1, sis.get_generation());

  Ok(())
}

#[test]
fn test_segments_inflation() -> Result<()> {
  let mut random = random();
  let dir = new_mock_directory(&mut random)?;
  dir.set_check_index_on_close(false);

  // empty commit
  let writer = IndexWriter::new(Arc::new(dir.clone()), IndexWriterConfig::new()?)?;
  writer.close()?;
  drop(writer);

  let mut sis = SegmentInfos::read_latest_commit(Arc::new(dir.clone()))?;
  assert_eq!(1, sis.get_generation());

  // add trash commit
  let mut output = dir.create_output(
    &format!("{}{}", IndexFileNames::SEGMENTS, "_2"),
    &new_io_context(&mut random)?,
  )?;
  output.close()?;

  // ensure inflation
  inflate_gens_test(&mut sis, dir.list_all()?, &get_default_info_stream())?;
  assert_eq!(2, sis.get_generation());

  // add another trash commit
  let mut output = dir.create_output(
    &format!("{}{}", IndexFileNames::SEGMENTS, "_4"),
    &new_io_context(&mut random)?,
  )?;
  output.close()?;
  inflate_gens_test(&mut sis, dir.list_all()?, &get_default_info_stream())?;
  assert_eq!(4, sis.get_generation());

  Ok(())
}

#[test]
fn test_segment_name_inflation() -> Result<()> {
  let mut random = random();
  let dir = new_mock_directory(&mut random)?;

  // empty commit
  let writer = IndexWriter::new(Arc::new(dir.clone()), IndexWriterConfig::new()?)?;
  writer.close()?;
  drop(writer);

  let mut sis = SegmentInfos::read_latest_commit(Arc::new(dir.clone()))?;
  assert_eq!(0, sis.counter);

  // no inflation
  inflate_gens_test(&mut sis, dir.list_all()?, &get_default_info_stream())?;
  assert_eq!(0, sis.counter);

  // add trash per-segment file
  let mut output = dir.create_output(
    &IndexFileNames::segment_file_name("_0", "", "foo"),
    &new_io_context(&mut random)?,
  )?;
  output.close()?;

  // ensure inflation
  inflate_gens_test(&mut sis, dir.list_all()?, &get_default_info_stream())?;
  assert_eq!(1, sis.counter);

  // add trash per-segment file
  let mut output = dir.create_output(
    &IndexFileNames::segment_file_name("_3", "", "foo"),
    &new_io_context(&mut random)?,
  )?;
  output.close()?;
  inflate_gens_test(&mut sis, dir.list_all()?, &get_default_info_stream())?;
  assert_eq!(4, sis.counter);

  // ensure we write _4 segment next
  let writer = IndexWriter::new(Arc::new(dir.clone()), IndexWriterConfig::new()?)?;
  writer.add_document(Document::new())?;
  writer.commit()?;
  writer.close()?;
  drop(writer);
  sis = SegmentInfos::read_latest_commit(Arc::new(dir.clone()))?;
  assert_eq!("_4", sis.info(0).unwrap().info.name.as_ref());
  assert_eq!(5, sis.counter);

  Ok(())
}

#[test]
fn test_generation_inflation() -> Result<()> {
  let mut random = random();
  let dir = new_mock_directory(&mut random)?;

  // initial commit
  let writer = IndexWriter::new(Arc::new(dir.clone()), IndexWriterConfig::new()?)?;
  writer.add_document(Document::new())?;
  writer.commit()?;
  writer.close()?;
  drop(writer);

  // no deletes: start at 1
  let mut sis = SegmentInfos::read_latest_commit(Arc::new(dir.clone()))?;
  assert_eq!(1, sis.info(0).unwrap().get_next_del_gen());

  // no inflation
  inflate_gens_test(&mut sis, dir.list_all()?, &get_default_info_stream())?;
  assert_eq!(1, sis.info(0).unwrap().get_next_del_gen());

  // add trash per-segment deletes file
  let mut output = dir.create_output(
    &IndexFileNames::file_name_from_generation("_0", "del", 2).unwrap(),
    &new_io_context(&mut random)?,
  )?;
  output.close()?;

  // ensure inflation
  inflate_gens_test(&mut sis, dir.list_all()?, &get_default_info_stream())?;
  assert_eq!(3, sis.info(0).unwrap().get_next_del_gen());

  Ok(())
}

#[test]
fn test_trashy_file() -> Result<()> {
  let mut random = random();
  let dir = new_mock_directory(&mut random)?;
  dir.set_check_index_on_close(false);

  // empty commit
  let writer = IndexWriter::new(Arc::new(dir.clone()), IndexWriterConfig::new()?)?;
  writer.close()?;
  drop(writer);

  let mut sis = SegmentInfos::read_latest_commit(Arc::new(dir.clone()))?;
  assert_eq!(1, sis.get_generation());

  // add trash file
  let mut output = dir.create_output(
    &format!("{}{}", IndexFileNames::SEGMENTS, "_"),
    &new_io_context(&mut random)?,
  )?;
  output.close()?;

  // no inflation
  inflate_gens_test(&mut sis, dir.list_all()?, &get_default_info_stream())?;
  assert_eq!(1, sis.get_generation());

  Ok(())
}

#[test]
fn test_trashy_gen_file() -> Result<()> {
  let mut random = random();
  let dir = new_mock_directory(&mut random)?;

  // initial commit
  let writer = IndexWriter::new(Arc::new(dir.clone()), IndexWriterConfig::new()?)?;
  writer.add_document(Document::new())?;
  writer.commit()?;
  writer.close()?;
  drop(writer);

  // no deletes: start at 1
  let mut sis = SegmentInfos::read_latest_commit(Arc::new(dir.clone()))?;
  assert_eq!(1, sis.info(0).unwrap().get_next_del_gen());

  // add trash file
  let mut output = dir.create_output("_1_A", &new_io_context(&mut random)?)?;
  output.close()?;

  // no inflation
  inflate_gens_test(&mut sis, dir.list_all()?, &get_default_info_stream())?;
  assert_eq!(1, sis.info(0).unwrap().get_next_del_gen());

  Ok(())
}

fn inflate_gens_test<D>(
  sis: &mut SegmentInfos<D>,
  files: Vec<String>,
  stream: &InfoStreamMT,
) -> Result<()>
where
  D: Directory,
{
  let mut filtered = Vec::with_capacity(files.len());
  for file in files {
    if CODEC_FILE_PATTERN.is_match(&file)
      || file.starts_with(IndexFileNames::SEGMENTS)
      || file.starts_with(IndexFileNames::PENDING_SEGMENTS)
    {
      filtered.push(file);
    }
  }
  inflate_gens(sis, filtered.iter(), stream)
}

#[test]
fn test_exc_in_dec_ref() -> Result<()> {
  // LUCENE-5919
  let mut random = random();
  let dir = Arc::new(new_mock_directory(&mut random)?);

  // disable slow things: we don't rely upon sleeps here.
  dir.set_throttling(Throttling::Never);
  dir.set_use_slow_open_closers(false);

  let do_fail_exc = Arc::new(AtomicBool::new(false));
  dir.fail_on(Box::new(FailInDecRef {
    do_fail: false,
    do_fail_exc: do_fail_exc.clone(),
  }));

  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  if let MergeSchedulerEnum::Concurrent(cms) = config.get_merge_scheduler() {
    let suppress_fake_fail = ConcurrentMergeScheduler::with_hook(
      ConcurrentMergeSchedulerHook::TestIndexFileDeleter(FakeFailConcurrentMergeScheduler),
    );
    suppress_fake_fail
      .set_max_merges_and_threads(cms.get_max_merge_count(), cms.get_max_thread_count())?;
    config.set_merge_scheduler(suppress_fake_fail);
  }

  let w = RandomIndexWriter::with_config(&mut random, dir.clone(), config);

  // Since we hit exc during merging, a partial
  // forceMerge can easily return when there are still
  // too many segments in the index:
  w.set_do_random_force_merge_assert(false);

  do_fail_exc.store(true, Ordering::SeqCst);
  let iters = at_least(&mut random, 1000);
  let mut field_types = HashMap::new();
  for _ in 0..iters {
    let result = if random.random_range(0..10) == 5 {
      w.commit(&mut random).map(|_| ())
    } else if random.random_range(0..10) == 7 {
      w.get_reader(&mut random).and_then(|reader| reader.close())
    } else {
      let mut doc = Document::new();
      doc.add(new_text_field(
        &mut random,
        "field",
        "some text",
        Store::No,
        &mut field_types,
      )?);
      w.add_document(&mut random, doc).map(|_| ())
    };
    if let Err(error) = result {
      let is_fake_fail = error.to_string().contains("fake fail")
        || error
          .get_suppressed()?
          .is_some_and(|cause| cause.to_string().contains("fake fail"))
        || matches!(error.kind(), LuceneErrorKind::AlreadyClosed(_))
          && w
            .w
            .get_tragic_exception()
            .get()
            .is_some_and(|cause| cause.to_string().contains("fake fail"));
      if !is_fake_fail {
        return Err(error);
      }
    }
  }

  do_fail_exc.store(false, Ordering::SeqCst);
  w.close(&mut random)?;
  dir.as_ref().close()?;
  Ok(())
}

// LUCENE-6835: make sure best-effort to not create an "apparently but not really" corrupt index
// is working:
#[test]
fn test_exc_in_delete_file() -> Result<()> {
  let mut random = random();
  let iters = at_least(&mut random, 10);
  for iter in 0..iters {
    if cfg!(feature = "test_log_verbose") {
      println!("TEST: iter={iter}");
    }
    let dir = Arc::new(new_mock_directory(&mut random)?);
    let do_fail_exc = Arc::new(AtomicBool::new(false));
    dir.fail_on(Box::new(FailInDeleteFile {
      do_fail: false,
      do_fail_exc: do_fail_exc.clone(),
    }));

    let mut config = new_index_writer_config(&mut random)?;
    config.set_merge_scheduler(SerialMergeScheduler::new());
    let w = RandomIndexWriter::with_config(&mut random, dir.clone(), config);
    w.add_document(&mut random, Document::new())?;

    // makes segments_1
    if cfg!(feature = "test_log_verbose") {
      println!("TEST: now commit");
    }
    w.commit(&mut random)?;

    w.add_document(&mut random, Document::new())?;
    do_fail_exc.store(true, Ordering::SeqCst);
    if cfg!(feature = "test_log_verbose") {
      println!("TEST: now close");
    }
    if let Err(error) = w.close(&mut random) {
      assert!(
        error.to_string().contains("a fake I/O error"),
        "unexpected error: {error}"
      );
      if cfg!(feature = "test_log_verbose") {
        eprintln!("TEST: got expected error: {error:?}");
      }
    } else if cfg!(feature = "test_log_verbose") {
      println!("TEST: no error (ok)");
    }
    do_fail_exc.store(false, Ordering::SeqCst);
    assert!(!w.w.is_open());

    for name in dir.list_all()? {
      if name.starts_with(IndexFileNames::SEGMENTS) {
        if cfg!(feature = "test_log_verbose") {
          println!("TEST: now read {name}");
        }
        SegmentInfos::read_commit(dir.clone(), &name)?;
      }
    }
    dir.as_ref().close()?;
  }
  Ok(())
}

struct FailInDecRef {
  do_fail: bool,
  do_fail_exc: Arc<AtomicBool>,
}

impl<D> Failure<D> for FailInDecRef
where
  D: Directory,
{
  fn eval_with_context(
    &mut self,
    dir: &MockDirectoryWrapper<D>,
    context: &FailureContext,
  ) -> Result<()> {
    if self.do_fail_exc.load(Ordering::SeqCst)
      && dir.state.random_state.lock().random_range(0..4) == 1
      && context.contains(ExecutionOwner::IndexFileDeleter, ExecutionMethod::DecRef)
    {
      return Err(LuceneError::illegal_state("fake fail"));
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

struct FailInDeleteFile {
  do_fail: bool,
  do_fail_exc: Arc<AtomicBool>,
}

impl<D> Failure<D> for FailInDeleteFile
where
  D: Directory,
{
  fn eval_with_context(
    &mut self,
    dir: &MockDirectoryWrapper<D>,
    context: &FailureContext,
  ) -> Result<()> {
    if self.do_fail_exc.load(Ordering::SeqCst)
      && dir.state.random_state.lock().random_range(0..4) == 1
      && context.point() == FailurePoint::DeleteFile
    {
      return Err(LuceneError::io(Error::other(FakeIOException)));
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

#[test]
fn test_throw_exception_while_delete_commits() -> Result<()> {
  let mut random = random();
  let dir = new_mock_directory(&mut random)?;
  let fail_on_delete_commits = Arc::new(AtomicBool::new(false));
  dir.fail_on(Box::new(FailOnDeleteCommits::new(
    fail_on_delete_commits.clone(),
  )));

  let snapshot_deletion_policy = SnapshotDeletionPolicy::new(KeepOnlyLastCommitDeletionPolicy);
  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config.set_index_deletion_policy(snapshot_deletion_policy.clone());

  let writer = IndexWriter::new(Arc::new(dir.clone()), config)?;
  writer.add_document(Document::new())?;
  writer.commit()?;

  let snapshot_commit = snapshot_deletion_policy.snapshot()?;
  let commits = random.random_range(1..=3);
  for _ in 0..commits {
    writer.add_document(Document::new())?;
    writer.commit()?;
  }
  snapshot_deletion_policy.release(&snapshot_commit)?;
  fail_on_delete_commits.store(true, Ordering::SeqCst);
  if let Err(error) = writer.delete_unused_files() {
    match error.kind() {
      LuceneErrorKind::Io { source, .. } | LuceneErrorKind::IoWithPath { source, .. } => {
        assert!(
          source
            .get_ref()
            .is_some_and(|source| source.is::<FakeIOException>()),
          "expected FakeIOException, got {source}"
        );
      },
      _ => return Err(error),
    }
  }
  fail_on_delete_commits.store(false, Ordering::SeqCst);
  for _ in 0..commits {
    writer.add_document(Document::new())?;
    writer.commit()?;
  }
  writer.close()?;
  Ok(())
}
struct FailOnDeleteCommits {
  do_fail: bool,
  fail_on_delete_commits: Arc<AtomicBool>,
}

impl FailOnDeleteCommits {
  fn new(fail_on_delete_commits: Arc<AtomicBool>) -> Self {
    Self {
      do_fail: true,
      fail_on_delete_commits,
    }
  }
}

impl<D> Failure<D> for FailOnDeleteCommits
where
  D: Directory,
{
  fn eval_with_context(
    &mut self,
    _dir: &MockDirectoryWrapper<D>,
    context: &FailureContext,
  ) -> Result<()> {
    if self.do_fail
      && self.fail_on_delete_commits.load(Ordering::SeqCst)
      && context.contains(
        ExecutionOwner::IndexFileDeleter,
        ExecutionMethod::DeleteCommits,
      )
      && context.point() == FailurePoint::DeleteFile
      && _dir.state.random_state.lock().random_range(0..4) == 1
    {
      return Err(LuceneError::io(Error::other(FakeIOException)));
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
