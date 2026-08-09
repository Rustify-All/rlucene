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
use crate::core::document::field::{Field, Store};
use crate::core::document::field_type::FieldType;
use crate::core::document::int_point::IntPoint;
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::index::concurrent_merge_scheduler::ConcurrentMergeScheduler;
use crate::core::index::directory_reader::{self, DirectoryReader};
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::OpenMode;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::log_merge_policy::LogMergePolicy;
use crate::core::index::merge_scheduler::MergeSchedulerEnum;
use crate::core::index::serial_merge_scheduler::SerialMergeScheduler;
use crate::core::index::term::Term;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::search::term_query::TermQuery;
use crate::core::store::ByteBuffersDirectory;
use crate::core::store::directory::Directory;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::index::merge_policy::KeepFullyDeletedSegmentsMergePolicy;
use crate::test_framework::core::store::mock_directory_wrapper::{Failure, MockDirectoryWrapper};
use crate::test_framework::core::util::lucene_test_case::{
  call_stack_contains_any_of, is_night_mode, new_directory_shared,
  new_index_writer_config_with_analyzer, new_log_merge_policy_with_cfs, new_mock_directory,
  new_searcher_with_reader, new_text_field, random,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::{Rng, RngExt};
use std::collections::HashMap;
use std::io::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[allow(dead_code)] // for quick search
struct TestIndexWriterOnDiskFull;

#[test]
fn test_add_document_on_disk_full() -> Result<()> {
  let mut random = random();
  let mut field_types = HashMap::new();

  for pass in 0..2 {
    let do_abort = pass == 1;
    let mut disk_free = TestUtil::next_int(&mut random, 100, 300) as i64;
    let mut index_exists = false;
    loop {
      let dir = Arc::new(MockDirectoryWrapper::new(
        &mut random,
        ByteBuffersDirectory::new(),
      ));
      dir.set_max_size_in_bytes(disk_free);
      let analyzer = MockAnalyzer::new(&mut random);
      let writer = IndexWriter::new(
        dir.clone(),
        new_index_writer_config_with_analyzer(&mut random, analyzer)?,
      )?;
      if let MergeSchedulerEnum::Concurrent(ms) = writer.get_config().get_merge_scheduler() {
        // This test intentionally produces exceptions
        // in the threads that CMS launches; we don't
        // want to pollute test output with these.
        ms.set_suppress_exceptions();
      }

      let mut hit_error = false;
      let add_result = (|| -> Result<()> {
        for _ in 0..200 {
          add_doc(&mut random, &writer, &mut field_types)?;
        }
        Ok(())
      })();
      match add_result {
        Ok(()) => match writer.commit() {
          Ok(_) => index_exists = true,
          Err(error)
            if error.is_io_error()
              || matches!(
                &error,
                LuceneError::IllegalState(_) | LuceneError::AlreadyClosed(_)
              ) =>
          {
            hit_error = true;
          },
          Err(error) => return Err(error),
        },
        Err(error) if error.is_io_error() => {
          hit_error = true;
        },
        Err(error) => return Err(error),
      }

      if hit_error {
        if do_abort {
          writer.rollback()?;
        } else {
          match writer.close() {
            Ok(()) => {},
            Err(error)
              if error.is_io_error()
                || matches!(
                  &error,
                  LuceneError::IllegalState(_) | LuceneError::AlreadyClosed(_)
                ) =>
            {
              dir.set_max_size_in_bytes(0);
              match writer.close() {
                Ok(()) | Err(LuceneError::AlreadyClosed(_)) => {},
                Err(error) => return Err(error),
              }
            },
            Err(error) => return Err(error),
          }
        }

        // _TestUtil.syncConcurrentMerges(ms);

        if index_exists {
          // Make sure reader can open the index:
          directory_reader::open(dir.clone())?.close()?;
        }

        dir.as_ref().close()?;
        // Now try again w/ more space:

        disk_free += if is_night_mode() {
          TestUtil::next_int(&mut random, 400, 600) as i64
        } else {
          TestUtil::next_int(&mut random, 3000, 5000) as i64
        };
      } else {
        // _TestUtil.syncConcurrentMerges(writer);
        dir.set_max_size_in_bytes(0);
        writer.close()?;
        dir.as_ref().close()?;
        break;
      }
    }
  }
  Ok(())
}

// TODO: make @Nightly variant that provokes more disk
// fulls

// TODO: have test fail if on any given top
// iter there was not a single IOE hit

/*
Test: make sure when we run out of disk space or hit
random IOExceptions in any of the addIndexes(*) calls
that 1) index is not corrupt (searcher can open/search
it) and 2) transactional semantics are followed:
either all or none of the incoming documents were in
fact added.
 */
#[test]
fn test_add_index_on_disk_full() -> Result<()> {
  // MemoryCodec, since it uses FST, is not necessarily
  // "additive", ie if you add up N small FSTs, then merge
  // them, the merged result can easily be larger than the
  // sum because the merged FST may use array encoding for
  // some arcs (which uses more space):

  const START_COUNT: i32 = 57;
  let num_dir = if is_night_mode() { 50 } else { 5 };
  let end_count = START_COUNT + num_dir * if is_night_mode() { 25 } else { 5 };
  let mut random = random();
  let mut field_types = HashMap::new();

  // Build up a bunch of dirs that have indexes which we
  // will then merge together by calling addIndexes(*):
  let mut dirs = Vec::with_capacity(num_dir as usize);
  let mut input_disk_usage = 0_i64;
  for i in 0..num_dir {
    let dir = new_directory_shared(&mut random)?;
    let analyzer = MockAnalyzer::new(&mut random);
    let writer = IndexWriter::new(
      dir.clone(),
      new_index_writer_config_with_analyzer(&mut random, analyzer)?,
    )?;
    for j in 0..25 {
      add_doc_with_index(&mut random, &writer, 25 * i + j, &mut field_types)?;
    }
    writer.close()?;
    for file in dir.list_all()? {
      input_disk_usage += dir.file_length(&file)? as i64;
    }
    dirs.push(dir);
  }

  // Now, build a starting index that has START_COUNT docs.  We
  // will then try to addIndexes into a copy of this:
  let start_dir = Arc::new(new_mock_directory(&mut random)?);
  let analyzer = MockAnalyzer::new(&mut random);
  let writer = IndexWriter::new(
    start_dir.clone(),
    new_index_writer_config_with_analyzer(&mut random, analyzer)?,
  )?;
  for j in 0..START_COUNT {
    add_doc_with_index(&mut random, &writer, j, &mut field_types)?;
  }
  writer.close()?;

  // Make sure starting index seems to be working properly:
  let search_term = Term::from_text("content", "aaa");
  let reader = directory_reader::open(start_dir.clone())?;
  assert_eq!(57, reader.doc_freq(&search_term)?, "first docFreq");

  let searcher = new_searcher_with_reader(reader)?;
  let hits = searcher
    .search(TermQuery::new(search_term.clone()), 1000)?
    .score_docs;
  assert_eq!(57, hits.len(), "first number of hits");
  searcher.get_index_reader().close()?;
  drop(searcher);

  // Iterate with larger and larger amounts of free
  // disk space.  With little free disk space,
  // addIndexes will certainly run out of space &
  // fail.  Verify that when this happens, index is
  // not corrupt and index in fact has added no
  // documents.  Then, we increase disk space by 2000
  // bytes each iteration.  At some point there is
  // enough free disk space and addIndexes should
  // succeed and index should show all documents were
  // added.

  // String[] files = startDir.listAll();
  let disk_usage = start_dir.size_in_bytes()? as i64;

  let mut start_disk_usage = 0_i64;
  for file in start_dir.list_all()? {
    start_disk_usage += start_dir.file_length(&file)? as i64;
  }

  for iter in 0..3 {
    // Start with 100 bytes more than we are currently using:
    let mut disk_free = disk_usage + TestUtil::next_int(&mut random, 50, 200) as i64;
    let method = iter;
    let mut done = false;

    let method_name = if method == 0 {
      "addIndexes(Directory[]) + forceMerge(1)"
    } else if method == 1 {
      "addIndexes(IndexReader[])"
    } else {
      "addIndexes(Directory[])"
    };

    while !done {
      // Make a new dir that will enforce disk usage:
      let copy = TestUtil::ram_copy_of(&mut random, start_dir.as_ref())?;
      let dir = Arc::new(MockDirectoryWrapper::new(&mut random, copy));
      let analyzer = MockAnalyzer::new(&mut random);
      let mut iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
      iwc.set_open_mode(OpenMode::Append);
      iwc.set_merge_policy(new_log_merge_policy_with_cfs(&mut random, false)?);
      let mut writer = IndexWriter::new(dir.clone(), iwc)?;
      let mut success: bool;

      for x in 0..2 {
        if let MergeSchedulerEnum::Concurrent(ms) = writer.get_config().get_merge_scheduler() {
          // This test intentionally produces exceptions
          // in the threads that CMS launches; we don't
          // want to pollute test output with these.
          if x == 0 {
            ms.set_suppress_exceptions();
          } else {
            ms.clear_suppress_exceptions();
          }
        }

        // Two loops: first time, limit disk space &
        // throw random IOExceptions; second time, no
        // disk space limit:
        let mut rate = 0.05;
        let disk_ratio = disk_free as f64 / disk_usage as f64;
        let this_disk_free;
        let test_name;

        if x == 0 {
          dir.set_random_io_exception_rate_on_open(random.random::<f64>() * 0.01);
          this_disk_free = disk_free;
          if disk_ratio >= 2.0 {
            rate /= 2.0;
          }
          if disk_ratio >= 4.0 {
            rate /= 2.0;
          }
          if disk_ratio >= 6.0 {
            rate = 0.0;
          }
          test_name = format!("disk full test {method_name} with disk full at {disk_free} bytes");
        } else {
          dir.set_random_io_exception_rate_on_open(0.0);
          this_disk_free = 0;
          rate = 0.0;
          test_name = format!("disk full test {method_name} with unlimited disk space");
        }

        dir.set_track_disk_usage(true);
        dir.set_max_size_in_bytes(this_disk_free);
        dir.set_random_io_exception_rate(rate);

        let operation_result = (|| -> Result<()> {
          if method == 0 {
            writer.add_indexes_from_directory(&dirs)?;
            writer.force_merge(1)?;
          } else if method == 1 {
            let mut readers = Vec::with_capacity(dirs.len());
            for input_dir in &dirs {
              readers.push(directory_reader::open(input_dir.clone())?);
            }
            let add_result = TestUtil::add_indexes_slowly(&writer, &readers);
            let close_result = (|| -> Result<()> {
              for reader in &readers {
                reader.close()?;
              }
              Ok(())
            })();
            close_result?;
            add_result?;
          } else {
            writer.add_indexes_from_directory(&dirs)?;
          }
          Ok(())
        })();

        match operation_result {
          Ok(()) => {
            success = true;
            if x == 0 {
              done = true;
            }
          },
          Err(error)
            if matches!(
              &error,
              LuceneError::IllegalState(_) | LuceneError::AlreadyClosed(_) | LuceneError::Merge(_)
            ) || error.is_io_error() =>
          {
            success = false;
            let is_merge_exception = matches!(error, LuceneError::Merge(_));
            if x == 1 && !is_merge_exception {
              return Err(LuceneError::illegal_state(format!(
                "{method_name} hit IOException after disk space was freed up"
              )));
            }
          },
          Err(error) => return Err(error),
        }

        if x == 1 {
          // Make sure all threads from ConcurrentMergeScheduler are done
          writer.wait_for_merges()?;
        } else {
          dir.set_random_io_exception_rate_on_open(0.0);
          writer.rollback()?;
          let analyzer = MockAnalyzer::new(&mut random);
          let mut iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
          iwc.set_open_mode(OpenMode::Append);
          iwc.set_merge_policy(new_log_merge_policy_with_cfs(&mut random, false)?);
          writer = IndexWriter::new(dir.clone(), iwc)?;
        }

        // Finally, verify index is not corrupt, and, if
        // we succeeded, we see all docs added, and if we
        // failed, we see either all docs or no docs added
        // (transactional semantics):
        dir.set_random_io_exception_rate_on_open(0.0);
        let reader = directory_reader::open(dir.clone()).map_err(|error| {
          LuceneError::illegal_state(format!(
            "{test_name}: exception when creating IndexReader: {error}"
          ))
        })?;
        let result = reader.doc_freq(&search_term)?;
        if success {
          if result != START_COUNT {
            return Err(LuceneError::illegal_state(format!(
              "{test_name}: method did not throw exception but docFreq('aaa') is {result} instead of expected {START_COUNT}"
            )));
          }
        } else if result != START_COUNT && result != end_count {
          return Err(LuceneError::illegal_state(format!(
            "{test_name}: method did throw exception but docFreq('aaa') is {result} instead of expected {START_COUNT} or {end_count}"
          )));
        }

        let searcher = new_searcher_with_reader(reader)?;
        let result2 = searcher
          .search(TermQuery::new(search_term.clone()), end_count as usize)
          .map_err(|error| {
            LuceneError::illegal_state(format!("{test_name}: exception when searching: {error}"))
          })?
          .score_docs
          .len() as i32;
        if success {
          if result2 != result {
            return Err(LuceneError::illegal_state(format!(
              "{test_name}: method did not throw exception but hits.length for search on term 'aaa' is {result2} instead of expected {result}"
            )));
          }
        } else if result2 != result {
          return Err(LuceneError::illegal_state(format!(
            "{test_name}: method did throw exception but hits.length for search on term 'aaa' is {result2} instead of expected {result}"
          )));
        }

        searcher.get_index_reader().close()?;
        if done || result == end_count {
          break;
        }
      }

      if done {
        // Javadocs state that temp free Directory space
        // required is at most 2X total input size of
        // indices so let's make sure:
        assert!(
          dir.get_max_used_size_in_bytes() - start_disk_usage
            < 2 * (start_disk_usage + input_disk_usage),
          "max free Directory space required exceeded 1X the total input index sizes during {method_name}: max temp usage = {} bytes vs limit={}; starting disk usage = {start_disk_usage} bytes; input index disk usage = {input_disk_usage} bytes",
          dir.get_max_used_size_in_bytes() - start_disk_usage,
          2 * (start_disk_usage + input_disk_usage)
        );
      }

      // Make sure we don't hit disk full during close below:
      dir.set_max_size_in_bytes(0);
      dir.set_random_io_exception_rate(0.0);
      dir.set_random_io_exception_rate_on_open(0.0);
      writer.close()?;
      dir.as_ref().close()?;

      // Try again with more free space:
      disk_free += if is_night_mode() {
        TestUtil::next_int(&mut random, 4000, 8000) as i64
      } else {
        TestUtil::next_int(&mut random, 40000, 80000) as i64
      };
    }
  }

  start_dir.as_ref().close()?;
  for dir in dirs {
    dir.as_ref().close()?;
  }
  Ok(())
}

#[derive(Clone)]
struct FailTwiceDuringMerge {
  do_fail: Arc<AtomicBool>,
  did_fail1: Arc<AtomicBool>,
  did_fail2: Arc<AtomicBool>,
}

impl FailTwiceDuringMerge {
  fn new() -> Self {
    Self {
      do_fail: Arc::new(AtomicBool::new(false)),
      did_fail1: Arc::new(AtomicBool::new(false)),
      did_fail2: Arc::new(AtomicBool::new(false)),
    }
  }

  fn set_do_fail(&self) {
    self.do_fail.store(true, Ordering::Relaxed);
  }

  fn clear_do_fail(&self) {
    self.do_fail.store(false, Ordering::Relaxed);
  }
}

impl<D> Failure<D> for FailTwiceDuringMerge
where
  D: Directory,
{
  fn eval(&mut self, _dir: &MockDirectoryWrapper<D>) -> Result<()> {
    if !self.do_fail.load(Ordering::Relaxed) {
      return Ok(());
    }
    if call_stack_contains_any_of(&["merge_terms"]) && !self.did_fail1.load(Ordering::Relaxed) {
      self.did_fail1.store(true, Ordering::Relaxed);
      return Err(LuceneError::io(Error::other(
        "fake disk full during mergeTerms",
      )));
    }
    if call_stack_contains_any_of(&["write_live_docs"])
      && call_stack_contains_any_of(&["merge"])
      && !self.did_fail2.load(Ordering::Relaxed)
    {
      self.did_fail2.store(true, Ordering::Relaxed);
      return Err(LuceneError::io(Error::other(
        "fake disk full while writing LiveDocs",
      )));
    }
    Ok(())
  }

  fn set_do_fail(&mut self) {
    self.do_fail.store(true, Ordering::Relaxed);
  }

  fn clear_do_fail(&mut self) {
    self.do_fail.store(false, Ordering::Relaxed);
  }
}

// LUCENE-2593
#[test]
fn test_corruption_after_disk_full_during_merge() -> Result<()> {
  let mut random = random();
  let dir = Arc::new(new_mock_directory(&mut random)?);
  // IndexWriter w = new IndexWriter(dir, newIndexWriterConfig(new
  // MockAnalyzer(random)).setReaderPooling(true));
  let mut mp = LogMergePolicy::log_doc();
  mp.set_merge_factor(2)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  config.set_merge_scheduler(SerialMergeScheduler::new());
  config.set_reader_pooling(true);
  config.set_merge_policy(KeepFullyDeletedSegmentsMergePolicy::new(mp));
  let w = IndexWriter::new(dir.clone(), config)?;
  let mut field_types = HashMap::new();
  let mut doc = Document::new();

  doc.add(new_text_field(
    &mut random,
    "f",
    "doctor who",
    Store::No,
    &mut field_types,
  )?);
  w.add_document(doc.clone())?;
  w.commit()?;
  w.delete_documents_with_terms(vec![Term::from_text("f", "who")])?;
  w.add_document(doc.clone())?;

  // disk fills up!
  let ftdm = FailTwiceDuringMerge::new();
  ftdm.set_do_fail();
  dir.fail_on(Box::new(ftdm.clone()));
  match w.commit() {
    Err(error) if error.is_io_error() => {},
    Err(error) => {
      return Err(LuceneError::illegal_state(format!(
        "expected IOException, got {error}"
      )));
    },
    Ok(_) => return Err(LuceneError::illegal_state("expected IOException")),
  }
  assert!(ftdm.did_fail1.load(Ordering::Relaxed) || ftdm.did_fail2.load(Ordering::Relaxed));

  TestUtil::check_index(&mut random, dir.as_ref())?;
  ftdm.clear_do_fail();
  match w.add_document(doc) {
    Err(LuceneError::AlreadyClosed(_)) => {},
    Err(error) => {
      return Err(LuceneError::illegal_state(format!(
        "expected AlreadyClosed, got {error}"
      )));
    },
    Ok(_) => return Err(LuceneError::illegal_state("expected AlreadyClosed")),
  }

  dir.as_ref().close()?;
  Ok(())
}

// LUCENE-1130: make sure immediate disk full on creating
// an IndexWriter (hit during DWPT#updateDocuments()) is
// OK:
#[test]
fn test_immediate_disk_full() -> Result<()> {
  let mut random = random();
  let dir = Arc::new(new_mock_directory(&mut random)?);
  let analyzer = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  config.set_max_buffered_docs(2);
  config.set_merge_scheduler(ConcurrentMergeScheduler::new());
  config.set_commit_on_close(false);
  let writer = IndexWriter::new(dir.clone(), config)?;
  writer.commit()?; // empty commit, to not create confusing situation with first commit
  dir.set_max_size_in_bytes(std::cmp::max(1, dir.size_in_bytes()? as i64));
  let mut doc = Document::new();
  let custom_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_STORED)?;
  doc.add(Field::new(
    "field",
    "aaa bbb ccc ddd eee fff ggg hhh iii jjj",
    custom_type,
  ));
  match writer.add_document(doc) {
    Err(error) if error.is_io_error() => {},
    Err(error) => {
      return Err(LuceneError::illegal_state(format!(
        "expected IOException, got {error}"
      )));
    },
    Ok(_) => return Err(LuceneError::illegal_state("expected IOException")),
  }
  assert!(writer.is_deleter_closed()?);
  assert!(writer.is_closed());

  dir.as_ref().close()?;
  Ok(())
}

// TODO: these are also in TestIndexWriter... add a simple doc-writing method
// like this to LuceneTestCase?
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
  doc.add(NumericDocValuesField::new("numericdv", 1));
  doc.add(IntPoint::new("point", [1])?);
  doc.add(IntPoint::new("point2d", [1, 1])?);
  writer.add_document(doc)?;
  Ok(())
}

fn add_doc_with_index<D, R>(
  random: &mut R,
  writer: &IndexWriter<D>,
  index: i32,
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
    format!("aaa {index}"),
    Store::No,
    field_types,
  )?);
  doc.add(new_text_field(
    random,
    "id",
    index.to_string(),
    Store::No,
    field_types,
  )?);
  doc.add(NumericDocValuesField::new("numericdv", 1));
  doc.add(IntPoint::new("point", [1])?);
  doc.add(IntPoint::new("point2d", [1, 1])?);
  writer.add_document(doc)?;
  Ok(())
}
