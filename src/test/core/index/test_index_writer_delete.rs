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
#[cfg(feature = "nightly")]
use crate::core::analysis::analyzer::{Analyzer, AnalyzerStoredValue, TokenStreamComponents};
#[cfg(feature = "nightly")]
use crate::core::analysis::token_stream::TokenStream;
use crate::core::document::document::Document;
use crate::core::document::field::{Field, Store};
use crate::core::document::field_type::FieldType;
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
#[cfg(feature = "nightly")]
use crate::core::document::stored_field::StoredField;
use crate::core::document::string_field::StringField;
use crate::core::document::text_field::TextField;
use crate::core::index::check_index::CheckIndex;
#[cfg(feature = "nightly")]
use crate::core::index::concurrent_merge_scheduler::ConcurrentMergeScheduler;
use crate::core::index::directory_reader;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::IndexWriter;
#[cfg(feature = "nightly")]
use crate::core::index::index_writer_config::DISABLE_AUTO_FLUSH;
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::log_merge_policy::LogMergePolicy;
use crate::core::index::merge_policy::MergePolicy;
use crate::core::index::serial_merge_scheduler::SerialMergeScheduler;
use crate::core::index::term::Term;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::search::term_query::TermQuery;
use crate::core::store::IndexInput;
use crate::core::store::directory::Directory;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::LuceneError;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::analysis::mock_tokenizer;
#[cfg(feature = "nightly")]
use crate::test_framework::core::analysis::mock_tokenizer::MockTokenizer;
use crate::test_framework::core::index::mock_random_merge_policy::MockRandomMergePolicy;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::index::test_index_writer::assert_no_unreferenced_files;
use crate::test_framework::core::store::mock_directory_wrapper::{Failure, MockDirectoryWrapper};
use crate::test_framework::core::util::lucene_test_case::random_from_seed;
#[cfg(feature = "nightly")]
use crate::test_framework::core::util::lucene_test_case::slow_file_exists;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, call_stack_contains_any_of, new_directory_shared, new_index_writer_config,
  new_index_writer_config_with_analyzer, new_log_merge_policy, new_mock_directory,
  new_searcher_with_reader, new_string_field, new_text_field, random,
};
#[cfg(feature = "nightly")]
use crate::test_framework::core::util::test_util::TestUtil;
use rand::RngExt;
use std::collections::HashMap;
#[cfg(feature = "nightly")]
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Barrier, Condvar, Mutex};
use std::thread;
use std::time::Duration;

#[allow(dead_code)] // for quick search
struct TestIndexWriterDelete;
#[test]
fn test_simple_case() -> Result<()> {
  let mut random = random();

  let keywords = ["1", "2"];
  let unindexed = ["Netherlands", "Italy"];
  let unstored = ["Amsterdam has lots of bridges", "Venice has lots of canals"];
  let text = ["Amsterdam", "Venice"];

  let dir = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::with_automaton(&mut random, mock_tokenizer::WHITESPACE.clone(), false);
  let iwc = new_index_writer_config_with_analyzer(&mut random, a)?;
  let modifier = IndexWriter::new(dir.clone(), iwc)?;

  let mut custom1 = FieldType::new();
  custom1.set_stored(true)?;
  custom1.freeze();

  for i in 0..keywords.len() {
    let mut doc = Document::new();
    doc.add(StringField::from_string("id", keywords[i], Store::Yes)?);
    doc.add(Field::new("country", unindexed[i], custom1.clone()));
    doc.add(TextField::from_string("contents", unstored[i], Store::No)?);
    doc.add(TextField::from_string("city", text[i], Store::Yes)?);
    modifier.add_document(doc)?;
  }

  modifier.force_merge(1)?;
  modifier.commit()?;

  let term = Term::from_text("city", "Amsterdam");
  let mut hit_count = get_hit_count(dir.clone(), term.clone())?;
  assert_eq!(1, hit_count);

  modifier.delete_documents_with_terms(vec![term.clone()])?;
  modifier.commit()?;

  hit_count = get_hit_count(dir.clone(), term)?;
  assert_eq!(0, hit_count);

  modifier.close()?;
  Ok(())
}
#[test]
fn test_non_ram_delete() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mut field_types = HashMap::new();

  let a = MockAnalyzer::with_automaton(&mut random, mock_tokenizer::WHITESPACE.clone(), false);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, a)?;
  iwc.set_max_buffered_docs(2);

  let modifier = IndexWriter::new(dir.clone(), iwc)?;

  let mut id = 0;
  let value = 100;

  for _ in 0..7 {
    id += 1;
    add_doc(&mut random, &modifier, id, value, &mut field_types)?;
  }

  modifier.commit()?;

  assert_eq!(0, modifier.get_num_buffered_documents());
  assert!(modifier.get_segment_count() > 0);

  modifier.commit()?;

  {
    let reader = directory_reader::open(dir.clone())?;
    assert_eq!(7, reader.num_docs()?);
  }

  modifier.delete_documents_with_terms(vec![Term::from_text("value", value.to_string())])?;

  modifier.commit()?;

  {
    let reader = directory_reader::open(dir.clone())?;
    assert_eq!(0, reader.num_docs()?);
  }

  modifier.close()?;
  Ok(())
}
// test when delete terms only apply to ram segments
#[test]
fn test_ram_deletes() -> Result<()> {
  let mut random = random();

  for t in 0..2 {
    if cfg!(feature = "test_log_verbose") {
      println!("TEST: t={t}");
    }
    let dir = new_directory_shared(&mut random)?;
    let mut field_types = HashMap::new();
    let analyzer =
      MockAnalyzer::with_automaton(&mut random, mock_tokenizer::WHITESPACE.clone(), false);
    let mut iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
    iwc.set_max_buffered_docs(4);
    let modifier = IndexWriter::new(dir.clone(), iwc)?;
    let mut id = 0;
    let value = 100;

    id += 1;
    add_doc(&mut random, &modifier, id, value, &mut field_types)?;
    if t == 0 {
      modifier.delete_documents_with_terms(vec![Term::from_text("value", value.to_string())])?;
    } else {
      modifier.delete_documents_with_queries(vec![
        TermQuery::new(Term::from_text("value", value.to_string())).into(),
      ])?;
    }
    id += 1;
    add_doc(&mut random, &modifier, id, value, &mut field_types)?;
    if t == 0 {
      modifier.delete_documents_with_terms(vec![Term::from_text("value", value.to_string())])?;
      assert_eq!(1, modifier.get_buffered_delete_terms_size()?);
    } else {
      modifier.delete_documents_with_queries(vec![
        TermQuery::new(Term::from_text("value", value.to_string())).into(),
      ])?;
    }

    id += 1;
    add_doc(&mut random, &modifier, id, value, &mut field_types)?;
    assert_eq!(0, modifier.get_segment_count());
    modifier.commit()?;

    let reader = directory_reader::open(dir.clone())?;
    assert_eq!(1, reader.num_docs()?);

    let hit_count = get_hit_count(dir.clone(), Term::from_text("id", id.to_string()))?;
    assert_eq!(1, hit_count);
    reader.close()?;
    modifier.close()?;
    dir.close()?;
  }

  Ok(())
}
#[test]
fn test_both_deletes() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mut field_types = HashMap::new();

  let a = MockAnalyzer::with_automaton(&mut random, mock_tokenizer::WHITESPACE.clone(), false);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, a)?;
  iwc.set_max_buffered_docs(100);

  let writer = IndexWriter::new(dir.clone(), iwc)?;

  let mut id = 0;
  let mut value = 100;

  // First 5 docs, value=100
  for _ in 0..5 {
    id += 1;
    add_doc(&mut random, &writer, id, value, &mut field_types)?;
  }

  value = 200;
  for _ in 0..5 {
    id += 1;
    add_doc(&mut random, &writer, id, value, &mut field_types)?;
  }

  writer.commit()?;

  for _ in 0..5 {
    id += 1;
    add_doc(&mut random, &writer, id, value, &mut field_types)?;
  }

  writer.delete_documents_with_terms(vec![Term::from_text("value", value.to_string())])?;
  writer.commit()?;

  {
    let reader = directory_reader::open(dir.clone())?;
    assert_eq!(5, reader.num_docs()?);
  }

  writer.close()?;
  Ok(())
}
#[test]
fn test_batch_deletes() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mut field_types = HashMap::new();

  let a = MockAnalyzer::with_automaton(&mut random, mock_tokenizer::WHITESPACE.clone(), false);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, a)?;
  iwc.set_max_buffered_docs(2);

  let modifier = IndexWriter::new(dir.clone(), iwc)?;

  let mut id = 0;
  let value = 100;

  for _ in 0..7 {
    id += 1;
    add_doc(&mut random, &modifier, id, value, &mut field_types)?;
  }

  modifier.commit()?;

  {
    let reader = directory_reader::open(dir.clone())?;
    assert_eq!(7, reader.num_docs()?);
  }

  id = 0;

  modifier.delete_documents_with_terms(vec![
    Term::from_text("id", (id + 1).to_string()),
    Term::from_text("id", (id + 2).to_string()),
  ])?;
  id += 2;

  modifier.commit()?;

  {
    let reader = directory_reader::open(dir.clone())?;
    assert_eq!(5, reader.num_docs()?);
  }

  let mut terms = Vec::new();
  for _ in 0..3 {
    id += 1;
    terms.push(Term::from_text("id", id.to_string()));
  }

  modifier.delete_documents_with_terms(terms)?;
  modifier.commit()?;

  {
    let reader = directory_reader::open(dir.clone())?;
    assert_eq!(2, reader.num_docs()?);
  }
  modifier.close()?;
  Ok(())
}
#[test]
fn test_delete_all_simple() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mut field_types = HashMap::new();

  let a = MockAnalyzer::with_automaton(&mut random, mock_tokenizer::WHITESPACE.clone(), false);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, a)?;
  iwc.set_max_buffered_docs(2);

  let modifier = IndexWriter::new(dir.clone(), iwc)?;

  let mut id = 0;
  let value = 100;

  for _ in 0..7 {
    id += 1;
    add_doc(&mut random, &modifier, id, value, &mut field_types)?;
  }
  modifier.commit()?;

  let reader = directory_reader::open(dir.clone())?;
  assert_eq!(7, reader.num_docs()?);
  reader.close()?;

  add_doc(&mut random, &modifier, 99, value, &mut field_types)?;

  modifier.delete_all()?;

  let reader = directory_reader::open(dir.clone())?;
  assert_eq!(7, reader.num_docs()?);
  reader.close()?;

  add_doc(&mut random, &modifier, 101, value, &mut field_types)?;
  update_doc(&mut random, &modifier, 102, value, &mut field_types)?;

  modifier.commit()?;

  let reader = directory_reader::open(dir.clone())?;
  assert_eq!(2, reader.num_docs()?);
  reader.close()?;

  modifier.close()?;
  Ok(())
}

#[test]
fn test_delete_all_no_dead_lock() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mut iwc = new_index_writer_config(&mut random)?;
  iwc.set_merge_policy(MockRandomMergePolicy::new(&mut random));
  let modifier = Arc::new(RandomIndexWriter::with_config(
    &mut random,
    dir.clone(),
    iwc,
  ));
  let num_threads = at_least(&mut random, 2) as usize;
  let latch = Arc::new(Barrier::new(num_threads + 1));
  let done_latch = Arc::new((Mutex::new(0_usize), Condvar::new()));
  let mut threads = Vec::new();

  for i in 0..num_threads {
    let modifier = modifier.clone();
    let latch = latch.clone();
    let done_latch = done_latch.clone();
    let seed = random.random();
    threads.push(thread::spawn(move || -> Result<()> {
      let mut thread_random = random_from_seed(seed);
      let mut id = (i as i32) * 1000;
      let value = 100;
      latch.wait();
      let result = (|| -> Result<()> {
        for _ in 0..1000 {
          let mut doc = Document::new();
          doc.add(TextField::from_string("content", "aaa", Store::No)?);
          doc.add(StringField::from_string("id", id.to_string(), Store::Yes)?);
          id += 1;
          doc.add(StringField::from_string(
            "value",
            value.to_string(),
            Store::No,
          )?);
          doc.add(NumericDocValuesField::new("dv", value as i64));
          modifier.add_document(&mut thread_random, doc)?;
        }
        Ok(())
      })();
      let (lock, cvar) = &*done_latch;
      *lock.lock().unwrap() += 1;
      cvar.notify_one();
      result
    }));
  }

  latch.wait();
  loop {
    let (lock, cvar) = &*done_latch;
    let done_count = lock.lock().unwrap();
    let (done_count, _) = cvar
      .wait_timeout_while(done_count, Duration::from_millis(1), |done_count| {
        *done_count < num_threads
      })
      .unwrap();
    if *done_count >= num_threads {
      break;
    }
    drop(done_count);
    modifier.w.delete_all()?;
  }

  modifier.w.delete_all()?;
  for thread in threads {
    match thread.join() {
      Ok(result) => result?,
      Err(e) => std::panic::resume_unwind(e),
    }
  }

  modifier.close(&mut random)?;

  let reader = directory_reader::open(dir.clone())?;
  assert_eq!(0, reader.max_doc()?);
  assert_eq!(0, reader.num_docs()?);
  assert_eq!(0, reader.num_deleted_docs()?);
  reader.close()?;

  Ok(())
}
#[test]
fn test_delete_all_rollback() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mut field_types = HashMap::new();

  let a = MockAnalyzer::with_automaton(&mut random, mock_tokenizer::WHITESPACE.clone(), false);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, a)?;
  iwc.set_max_buffered_docs(2);

  let modifier = IndexWriter::new(dir.clone(), iwc)?;

  let mut id = 0;
  let value = 100;

  for _ in 0..7 {
    id += 1;
    add_doc(&mut random, &modifier, id, value, &mut field_types)?;
  }
  modifier.commit()?;

  id += 1;
  add_doc(&mut random, &modifier, id, value, &mut field_types)?;

  let reader = directory_reader::open(dir.clone())?;
  assert_eq!(7, reader.num_docs()?);
  reader.close()?;

  modifier.delete_all()?;

  modifier.rollback()?;

  let reader = directory_reader::open(dir.clone())?;
  assert_eq!(7, reader.num_docs()?);
  reader.close()?;

  Ok(())
}
#[test]
fn test_delete_all_nrt() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mut field_types = HashMap::new();

  let a = MockAnalyzer::with_automaton(&mut random, mock_tokenizer::WHITESPACE.clone(), false);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, a)?;
  iwc.set_max_buffered_docs(2);

  let modifier = IndexWriter::new(dir.clone(), iwc)?;

  let mut id = 0;
  let value = 100;

  for _ in 0..7 {
    id += 1;
    add_doc(&mut random, &modifier, id, value, &mut field_types)?;
  }
  modifier.commit()?;

  let reader = directory_reader::open_from_writer(&modifier)?;
  assert_eq!(7, reader.num_docs()?);
  reader.close()?;

  id += 1;
  add_doc(&mut random, &modifier, id, value, &mut field_types)?;
  id += 1;
  add_doc(&mut random, &modifier, id, value, &mut field_types)?;

  modifier.delete_all()?;

  let reader = directory_reader::open_from_writer(&modifier)?;
  assert_eq!(0, reader.num_docs()?);
  reader.close()?;

  modifier.rollback()?;

  let reader = directory_reader::open(dir.clone())?;
  assert_eq!(7, reader.num_docs()?);
  reader.close()?;

  Ok(())
}
#[cfg(feature = "nightly")]
#[test]
#[ignore = "nightly"]
fn test_delete_all_repeated() -> Result<()> {
  let breaking_field_count = 50_000_000_i64;
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mut conf = new_index_writer_config(&mut random)?;
  conf.set_max_buffered_docs(1000);
  conf.set_ram_buffer_size_mb(1000.0);
  conf.get_base_mut().per_thread_hard_limit_mb = 1000;
  conf.set_check_pending_flush_update(false);

  let modifier = IndexWriter::new(dir.clone(), conf)?;
  let fields_per_doc = 1_000_i64;
  let num_fields = Arc::new(AtomicI64::new(0));
  let n_threads = at_least(&mut random, 8) as usize;
  let mut threads = Vec::new();

  for _ in 0..n_threads {
    let modifier = modifier.clone();
    let num_fields = num_fields.clone();
    threads.push(thread::spawn(move || -> Result<()> {
      while num_fields.fetch_add(fields_per_doc, Ordering::SeqCst) < breaking_field_count {
        let mut document = Document::new();
        for i in 0..fields_per_doc {
          document.add(StoredField::from_string(format!("field{i}"), "")?);
        }
        modifier.add_document(document)?;
        modifier.delete_all()?;
      }
      Ok(())
    }));
  }

  for thread in threads {
    match thread.join() {
      Ok(result) => result?,
      Err(e) => std::panic::resume_unwind(e),
    }
  }

  let mut document = Document::new();
  for i in 0..fields_per_doc {
    document.add(StoredField::from_string(format!("field{i}"), "")?);
  }
  modifier.add_document(document)?;
  modifier.flush()?;
  modifier.close()?;

  Ok(())
}
fn update_doc<D, R>(
  random: &mut R,
  modifier: &IndexWriter<D>,
  id: i32,
  value: i32,
  field_types: &mut HashMap<String, FieldType>,
) -> Result<()>
where
  D: Directory + 'static,
  R: rand::Rng + ?Sized,
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
    Store::Yes,
    field_types,
  )?);
  doc.add(new_string_field(
    random,
    "value",
    value.to_string(),
    Store::No,
    field_types,
  )?);
  doc.add(NumericDocValuesField::new("dv", value as i64));

  modifier.update_document_with_term(Term::from_text("id", id.to_string()), doc)?;
  Ok(())
}

fn add_doc<D, R>(
  random: &mut R,
  modifier: &IndexWriter<D>,
  id: i32,
  value: i32,
  field_types: &mut HashMap<String, FieldType>,
) -> Result<()>
where
  D: Directory + 'static,
  R: rand::Rng + ?Sized,
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
    Store::Yes,
    field_types,
  )?);
  doc.add(new_string_field(
    random,
    "value",
    value.to_string(),
    Store::No,
    field_types,
  )?);
  doc.add(NumericDocValuesField::new("dv", value as i64));

  modifier.add_document(doc)?;
  Ok(())
}

fn get_hit_count<D>(dir: Arc<D>, term: Term) -> Result<i64>
where
  D: Directory + 'static + std::marker::Send + Sync,
  <<D as Directory>::IndexInput as IndexInput>::RandomAccessSlice: Send + Sync,
  <D as Directory>::IndexInput: Send + Sync,
{
  let reader = directory_reader::open(dir)?;
  let searcher = new_searcher_with_reader(reader)?;
  let top_docs = searcher.search(TermQuery::new(term.clone()), 1000)?;
  let hit_count = top_docs.total_hits.value() as i64;
  searcher.get_index_reader().close()?;
  Ok(hit_count)
}
// TODO: can we fix MockDirectoryWrapper disk full checking to be more efficient (not recompute on
// every write)?
#[cfg(feature = "nightly")]
#[test]
#[ignore = "nightly"]
fn test_deletes_on_disk_full() -> Result<()> {
  do_test_operations_on_disk_full(false)
}

// TODO: can we fix MockDirectoryWrapper disk full checking to be more efficient (not recompute on
// every write)?
#[cfg(feature = "nightly")]
#[test]
#[ignore = "nightly"]
fn test_updates_on_disk_full() -> Result<()> {
  do_test_operations_on_disk_full(true)
}

/// Make sure if modifier tries to commit but hits disk full that modifier
/// remains consistent and usable. Similar to TestIndexReader.testDiskFull().
#[cfg(feature = "nightly")]
fn do_test_operations_on_disk_full(updates: bool) -> Result<()> {
  let mut random = random();
  let search_term = Term::from_text("content", "aaa");
  const START_COUNT: i64 = 157;
  const END_COUNT: i64 = 144;

  // First build up a starting index:
  let start_dir =
    Arc::new(crate::test_framework::core::util::lucene_test_case::new_mock_directory(&mut random)?);

  let analyzer =
    MockAnalyzer::with_automaton(&mut random, mock_tokenizer::WHITESPACE.clone(), false);
  let writer = IndexWriter::new(
    start_dir.clone(),
    new_index_writer_config_with_analyzer(&mut random, analyzer)?,
  )?;
  for i in 0..157 {
    let mut document = Document::new();
    document.add(StringField::from_string("id", i.to_string(), Store::Yes)?);
    document.add(TextField::from_string(
      "content",
      format!("aaa {i}"),
      Store::No,
    )?);
    document.add(NumericDocValuesField::new("dv", i as i64));
    writer.add_document(document)?;
  }
  writer.close()?;

  let disk_usage = start_dir.size_in_bytes()? as i64;
  let mut disk_free = disk_usage + 10;
  let mut err = None;
  let mut done = false;

  // Iterate w/ ever-increasing free disk space:
  while !done {
    let copy = TestUtil::ram_copy_of(&mut random, start_dir.as_ref())?;
    let dir = Arc::new(MockDirectoryWrapper::new(&mut random, copy));
    dir.set_allow_random_file_not_found_exception(false);
    let analyzer =
      MockAnalyzer::with_automaton(&mut random, mock_tokenizer::WHITESPACE.clone(), false);
    let mut config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
    config.set_max_buffered_docs(1000);
    let merge_scheduler = ConcurrentMergeScheduler::new();
    merge_scheduler.set_suppress_exceptions();
    config.set_merge_scheduler(merge_scheduler);
    let modifier = IndexWriter::new(dir.clone(), config)?;

    // For each disk size, first try to commit against dir that will hit random
    // IOExceptions & disk full; after, give it infinite disk space & turn off
    // random IOExceptions & retry w/ same reader:
    let mut success = false;

    for x in 0..2 {
      let mut rate = 0.1;
      let disk_ratio = disk_free as f64 / disk_usage as f64;
      let (this_disk_free, test_name) = if x == 0 {
        if disk_ratio >= 2.0 {
          rate /= 2.0;
        }
        if disk_ratio >= 4.0 {
          rate /= 2.0;
        }
        if disk_ratio >= 6.0 {
          rate = 0.0;
        }
        dir.set_random_io_exception_rate_on_open(random.random::<f64>() * 0.01);
        (
          disk_free,
          format!("disk full during reader.close() @ {disk_free} bytes"),
        )
      } else {
        rate = 0.0;
        dir.set_random_io_exception_rate_on_open(0.0);
        (0, "reader re-use after disk full".to_string())
      };

      dir.set_max_size_in_bytes(this_disk_free);
      dir.set_random_io_exception_rate(rate);

      let operation_result = (|| -> Result<()> {
        if x == 0 {
          let mut doc_id = 12;
          for i in 0..13 {
            if updates {
              let mut document = Document::new();
              document.add(StringField::from_string("id", i.to_string(), Store::Yes)?);
              document.add(TextField::from_string(
                "content",
                format!("bbb {i}"),
                Store::No,
              )?);
              document.add(NumericDocValuesField::new("dv", i as i64));
              modifier
                .update_document_with_term(Term::from_text("id", doc_id.to_string()), document)?;
            } else {
              modifier
                .delete_documents_with_terms(vec![Term::from_text("id", doc_id.to_string())])?;
            }
            doc_id += 12;
          }
          match modifier.close() {
            Ok(()) => {},
            Err(LuceneError::IllegalState(mut error)) => {
              // ok
              if let Some(cause) = error.source.take() {
                return Err(*cause);
              }
              return Err(LuceneError::IllegalState(error));
            },
            Err(error) => return Err(error),
          }
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
        Err(error @ (LuceneError::Io { .. } | LuceneError::IoWithPath { .. })) => {
          err = Some(error);
          if x == 1 {
            return Err(LuceneError::illegal_state(format!(
              "{test_name} hit IOException after disk space was freed up"
            )));
          }
        },
        Err(error) => return Err(error),
      }

      // prevent throwing a random exception here!!
      let random_io_exception_rate = dir.get_random_io_exception_rate();
      let max_size_in_bytes = dir.get_max_size_in_bytes();
      dir.set_random_io_exception_rate(0.0);
      dir.set_random_io_exception_rate_on_open(0.0);
      dir.set_max_size_in_bytes(0);
      if !success {
        // Must force the close else the writer can have open files which cause
        // exc in MockRAMDir.close
        modifier.rollback()?;
      }

      // If the close() succeeded, make sure index is OK:
      if success {
        TestUtil::check_index(&mut random, dir.as_ref())?;
      }
      dir.set_random_io_exception_rate(random_io_exception_rate);
      dir.set_max_size_in_bytes(max_size_in_bytes);

      // Finally, verify index is not corrupt, and, if we succeeded, we see all
      // docs changed, and if we failed, we see either all docs or no docs
      // changed (transactional semantics):
      let new_reader = directory_reader::open(dir.clone()).map_err(|error| {
        LuceneError::illegal_state(format!(
          "{test_name}:exception when creating IndexReader after disk full during close: {error}"
        ))
      })?;
      let searcher = new_searcher_with_reader(new_reader)?;
      let hits = searcher
        .search(TermQuery::new(search_term.clone()), 1000)
        .map_err(|error| {
          LuceneError::illegal_state(format!("{test_name}: exception when searching: {error}"))
        })?
        .score_docs;
      let result2 = hits.len() as i64;
      if success {
        if x == 0 && result2 != END_COUNT {
          return Err(LuceneError::illegal_state(format!(
            "{test_name}: method did not throw exception but hits.length for search on term 'aaa' is {result2} instead of expected {END_COUNT}"
          )));
        } else if x == 1 && result2 != START_COUNT && result2 != END_COUNT {
          // It's possible that the first exception was "recoverable" wrt
          // pending deletes, in which case the pending deletes are retained
          // and then re-flushing (with plenty of disk space) will succeed in
          // flushing the deletes:
          return Err(LuceneError::illegal_state(format!(
            "{test_name}: method did not throw exception but hits.length for search on term 'aaa' is {result2} instead of expected {START_COUNT} or {END_COUNT}"
          )));
        }
      } else {
        // On hitting exception we still may have added all docs:
        if result2 != START_COUNT && result2 != END_COUNT {
          return Err(LuceneError::illegal_state(format!(
            "{test_name}: method did throw exception but hits.length for search on term 'aaa' is {result2} instead of expected {START_COUNT} or {END_COUNT}: {err:?}"
          )));
        }
      }
      searcher.get_index_reader().close()?;
      if result2 == END_COUNT {
        break;
      }
    }
    dir.as_ref().close()?;

    // Try again with more bytes of free space:
    disk_free += std::cmp::max(10, disk_free >> 3);
  }
  start_dir.as_ref().close()
}

#[ignore]
#[test]
fn test_error_after_apply_deletes() -> Result<()> {
  // This test tests that buffered deletes are cleared when
  // an Exception is hit during flush.
  let mut random = random();
  let dir = Arc::new(new_mock_directory(&mut random)?);
  let mut failure: Box<dyn Failure<_>> = Box::new(FailAfterApplyDeletes {
    do_fail: false,
    saw_maybe: false,
    failed: false,
    thread: thread::current().id(),
  });
  failure.reset();
  dir.fail_on(failure);

  // create a couple of files
  let keywords = ["1", "2"];
  let unindexed = ["Netherlands", "Italy"];
  let unstored = ["Amsterdam has lots of bridges", "Venice has lots of canals"];
  let text = ["Amsterdam", "Venice"];

  let analyzer =
    MockAnalyzer::with_automaton(&mut random, mock_tokenizer::WHITESPACE.clone(), false);
  let mut merge_policy = new_log_merge_policy(&mut random)?;
  merge_policy.get_base_mut().set_no_cfs_ratio(1.0)?;
  let mut config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  config
    .set_reader_pooling(false)
    .set_merge_policy(merge_policy);
  let modifier = IndexWriter::new(dir.clone(), config)?;

  let mut custom1 = FieldType::new();
  custom1.set_stored(true)?;
  custom1.freeze();
  for i in 0..keywords.len() {
    let mut doc = Document::new();
    doc.add(StringField::from_string("id", keywords[i], Store::Yes)?);
    doc.add(Field::new("country", unindexed[i], custom1.clone()));
    doc.add(TextField::from_string("contents", unstored[i], Store::No)?);
    doc.add(TextField::from_string("city", text[i], Store::Yes)?);
    modifier.add_document(doc)?;
  }
  // flush

  if cfg!(feature = "test_log_verbose") {
    println!("TEST: now full merge");
  }
  modifier.force_merge(1)?;
  if cfg!(feature = "test_log_verbose") {
    println!("TEST: now commit");
  }
  modifier.commit()?;

  // one of the two files hits
  let term = Term::from_text("city", "Amsterdam");
  let mut hit_count = get_hit_count(dir.clone(), term.clone())?;
  assert_eq!(1, hit_count);

  // delete the doc
  // max buf del terms is two, so this is buffered
  if cfg!(feature = "test_log_verbose") {
    println!("TEST: delete term={term}");
  }
  modifier.delete_documents_with_terms(vec![term.clone()])?;

  // add a doc,
  // doc remains buffered
  if cfg!(feature = "test_log_verbose") {
    println!("TEST: add empty doc");
  }
  modifier.add_document(Document::new())?;

  // commit the changes, the buffered deletes, and the new doc

  // The failure object will fail on the first write after the del
  // file gets created when processing the buffered delete

  // in the ac case, this will be when writing the new segments
  // files so we really don't need the new doc, but it's harmless

  // a new segments file won't be created but in this
  // case, creation of the cfs file happens next so we
  // need the doc (to test that it's okay that we don't
  // lose deletes if failing while creating the cfs file)
  if cfg!(feature = "test_log_verbose") {
    println!("TEST: now commit for failure");
  }
  let expected = modifier
    .commit()
    .expect_err("commit should fail after applying deletes");
  assert!(
    expected.to_string().contains("fail after applyDeletes"),
    "unexpected error: {expected}"
  );

  // The commit above failed, so we need to retry it (which will
  // succeed, because the failure is a one-shot)
  let writer_closed = match modifier.commit() {
    Ok(_) => false,
    Err(LuceneError::IllegalState(_)) | Err(LuceneError::AlreadyClosed(_)) => true,
    Err(error) => return Err(error),
  };

  if !writer_closed {
    hit_count = get_hit_count(dir.clone(), term)?;

    // Make sure the delete was successfully flushed:
    assert_eq!(0, hit_count);

    modifier.close()?;
  }
  dir.as_ref().close()?;
  Ok(())
}

struct FailAfterApplyDeletes {
  do_fail: bool,
  saw_maybe: bool,
  failed: bool,
  thread: thread::ThreadId,
}

impl<D> Failure<D> for FailAfterApplyDeletes
where
  D: Directory,
{
  fn reset(&mut self) {
    self.thread = thread::current().id();
    self.saw_maybe = false;
    self.failed = false;
  }

  fn eval(&mut self, _dir: &MockDirectoryWrapper<D>) -> Result<()> {
    if thread::current().id() != self.thread {
      // don't fail during merging
      return Ok(());
    }
    if cfg!(feature = "test_log_verbose") {
      println!("FAIL EVAL:");
    }
    if self.saw_maybe && !self.failed {
      let seen = call_stack_contains_any_of(&["apply_all_deletes_and_updates", "slow_file_exists"]);
      if !seen {
        // Only fail once we are no longer in applyDeletes
        self.failed = true;
        if cfg!(feature = "test_log_verbose") {
          println!("TEST: mock failure: now fail");
        }
        return Err(LuceneError::illegal_state("fail after applyDeletes"));
      }
    }
    if !self.failed && call_stack_contains_any_of(&["apply_all_deletes_and_updates"]) {
      if cfg!(feature = "test_log_verbose") {
        println!("TEST: mock failure: saw applyDeletes");
      }
      self.saw_maybe = true;
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

struct FailInDocsWriterAdd {
  do_fail: bool,
  failed: bool,
}

impl<D> Failure<D> for FailInDocsWriterAdd
where
  D: Directory,
{
  fn reset(&mut self) {
    self.failed = false;
  }

  fn eval(&mut self, _dir: &MockDirectoryWrapper<D>) -> Result<()> {
    if !self.failed {
      self.failed = true;
      return Err(LuceneError::io(std::io::Error::other("fail in add doc")));
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
fn test_error_in_docs_writer_add() -> Result<()> {
  let mut failure: Box<dyn Failure<_>> = Box::new(FailInDocsWriterAdd {
    do_fail: false,
    failed: false,
  });

  // create a couple of files
  let keywords = ["1", "2"];
  let unindexed = ["Netherlands", "Italy"];
  let unstored = ["Amsterdam has lots of bridges", "Venice has lots of canals"];
  let text = ["Amsterdam", "Venice"];

  let mut random = random();
  let dir = Arc::new(new_mock_directory(&mut random)?);
  let analyzer =
    MockAnalyzer::with_automaton(&mut random, mock_tokenizer::WHITESPACE.clone(), false);
  let config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  let modifier = IndexWriter::new(dir.clone(), config)?;
  modifier.commit()?;
  failure.reset();
  dir.fail_on(failure);

  let mut custom1 = FieldType::new();
  custom1.set_stored(true)?;
  for i in 0..keywords.len() {
    let mut doc = Document::new();
    doc.add(StringField::from_string("id", keywords[i], Store::Yes)?);
    doc.add(Field::new("country", unindexed[i], custom1.clone()));
    doc.add(TextField::from_string("contents", unstored[i], Store::No)?);
    doc.add(TextField::from_string("city", text[i], Store::Yes)?);
    match modifier.add_document(doc) {
      Ok(_) => {},
      Err(error @ LuceneError::Io { .. }) | Err(error @ LuceneError::IoWithPath { .. }) => {
        if cfg!(feature = "test_log_verbose") {
          println!("TEST: got expected exc:\n{error:?}");
        }
        break;
      },
      Err(error) => return Err(error),
    }
  }
  assert!(modifier.is_deleter_closed()?);

  assert_no_unreferenced_files(
    dir.clone(),
    "docsWriter.abort() failed to delete unreferenced files",
  )?;
  dir.as_ref().close()?;
  Ok(())
}

#[test]
fn test_delete_null_query() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::with_automaton(&mut random, mock_tokenizer::WHITESPACE.clone(), false);
  let iwc = IndexWriterConfig::with_analyzer(a)?;
  let modifier = IndexWriter::new(dir, iwc)?;
  let mut field_types = HashMap::new();

  for i in 0..5 {
    add_doc(&mut random, &modifier, i, 2 * i, &mut field_types)?;
  }

  modifier
    .delete_documents_with_queries(vec![TermQuery::new(Term::from_text("nada", "nada")).into()])?;
  modifier.commit()?;
  assert_eq!(5, modifier.get_doc_stats()?.num_docs);
  modifier.close()?;
  Ok(())
}

#[test]
fn test_delete_all_slowly() -> Result<()> {
  use crate::core::index::index_reader::IndexReader;
  use rand::RngExt;
  use rand::seq::SliceRandom;

  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let iwc = new_index_writer_config(&mut random)?;
  let w = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);

  let num_docs = at_least(&mut random, 1000) as usize;
  let mut ids: Vec<i32> = (0..num_docs as i32).collect();
  ids.shuffle(&mut random);
  let mut field_types = HashMap::new();

  for &id in &ids {
    let mut doc = Document::new();
    doc.add(new_string_field(
      &mut random,
      "id",
      id.to_string(),
      Store::No,
      &mut field_types,
    )?);
    w.add_document(&mut random, doc)?;
  }
  ids.shuffle(&mut random);

  let mut upto = 0;
  while upto < ids.len() {
    let left = ids.len() - upto;
    let inc = std::cmp::min(left, random.random_range(1..21));
    let limit = upto + inc;
    while upto < limit {
      w.delete_documents_with_terms(
        &mut random,
        vec![Term::from_text("id", ids[upto].to_string())],
      )?;
      upto += 1;
    }
    let r = w.get_reader(&mut random)?;
    assert_eq!((num_docs - upto) as i32, r.num_docs()?);
    r.close()?;
  }

  w.close(&mut random)?;

  Ok(())
}
#[cfg(feature = "nightly")]
#[test]
#[ignore = "nightly"]
fn test_indexing_then_deleting() -> Result<()> {
  use rand::RngExt;

  struct IndexingThenDeletingAnalyzer {
    stored_value: AnalyzerStoredValue,
    seed: u64,
  }

  impl Analyzer for IndexingThenDeletingAnalyzer {
    fn create_components(&self, _field_name: &str) -> Result<TokenStreamComponents> {
      let tokenizer = MockTokenizer::with_default_max_token_length(
        random_from_seed(self.seed),
        mock_tokenizer::WHITESPACE.clone(),
        true,
      );
      Ok(TokenStreamComponents::new(
        Box::new(tokenizer) as Box<dyn TokenStream + Send + Sync>,
        None,
      ))
    }

    fn stored_value(&self) -> &AnalyzerStoredValue {
      &self.stored_value
    }
  }
  crate::impl_analyzer_close!(IndexingThenDeletingAnalyzer);

  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = IndexingThenDeletingAnalyzer {
    stored_value: AnalyzerStoredValue::new(),
    seed: random.random(),
  };
  let mut iwc =
    new_index_writer_config_with_analyzer(&mut random, Box::new(analyzer) as Box<dyn Analyzer>)?;
  iwc
    .set_ram_buffer_size_mb(4.0)
    .set_max_buffered_docs(DISABLE_AUTO_FLUSH);
  let writer = IndexWriter::new(dir, iwc)?;

  let mut doc = Document::new();
  let mut field_types = HashMap::new();
  doc.add(new_text_field(
    &mut random,
    "field",
    "go 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20",
    Store::No,
    &mut field_types,
  )?);
  let num = at_least(&mut random, 1);
  for _ in 0..num {
    let mut count = 0;

    let do_indexing = random.random_bool(0.5);
    if do_indexing {
      let start_flush_count = writer.get_flush_count();
      while writer.get_flush_count() == start_flush_count {
        writer.add_document(doc.clone())?;
        count += 1;
      }
    } else {
      let start_flush_count = writer.get_flush_count();
      while writer.get_flush_count() == start_flush_count {
        writer.delete_documents_with_terms(vec![Term::from_text("foo", count.to_string())])?;
        count += 1;
      }
    }
    assert!(
      count > 2500,
      "flush happened too quickly during {} count={count}",
      if do_indexing { "indexing" } else { "deleting" }
    );
  }
  writer.close()?;

  Ok(())
}

#[test]
fn test_flush_pushed_deletes_by_ram() -> Result<()> {
  use crate::core::index::no_merge_policy::NoMergePolicy;
  use crate::test_framework::core::util::lucene_test_case::{
    new_directory_shared, new_index_writer_config_with_analyzer, slow_file_exists,
  };

  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  iwc
    .set_ram_buffer_size_mb(0.5)
    .set_max_buffered_docs(1000)
    .set_merge_policy(NoMergePolicy::default());
  iwc.set_reader_pooling(false);
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let mut count = 0;
  loop {
    let mut doc = Document::new();
    doc.add(StringField::from_string(
      "id",
      count.to_string(),
      Store::No,
    )?);
    let del_term = if count == 1010 {
      Term::from_text("id", "0")
    } else {
      Term::from_text("id", format!("x{}", count))
    };
    w.update_document_with_term(del_term, doc)?;
    if slow_file_exists(dir.as_ref(), "_0_1.del")? || slow_file_exists(dir.as_ref(), "_0_1.liv")? {
      break;
    }
    count += 1;
    if count > 100000 {
      unreachable!("delete's were not applied");
    }
  }
  w.close()?;

  Ok(())
}
#[cfg(feature = "nightly")]
#[test]
#[ignore = "nightly"]
fn test_apply_deletes_on_flush() -> Result<()> {
  use crate::core::index::index_writer::{IndexWriterHooks, IndexWriterHooksEnum};
  use crate::core::index::no_merge_policy::NoMergePolicy;
  use crate::test_framework::core::util::lucene_test_case::{
    new_directory_shared, new_index_writer_config_with_analyzer,
  };
  use crate::test_framework::core::util::test_util::TestUtil;
  use std::sync::atomic::{AtomicBool, AtomicI32};

  struct ApplyDeletesOnFlushHooks {
    docs_in_segment: Arc<AtomicI32>,
    closing: Arc<AtomicBool>,
    saw_after_flush: Arc<AtomicBool>,
  }

  impl IndexWriterHooks for ApplyDeletesOnFlushHooks {
    fn do_after_flush(&self) -> Result<()> {
      let docs_in_segment = self.docs_in_segment.load(Ordering::SeqCst);
      assert!(
        self.closing.load(Ordering::SeqCst) || docs_in_segment >= 7,
        "only {docs_in_segment} in segment"
      );
      self.docs_in_segment.store(0, Ordering::SeqCst);
      self.saw_after_flush.store(true, Ordering::SeqCst);
      Ok(())
    }
  }

  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let docs_in_segment = Arc::new(AtomicI32::new(0));
  let closing = Arc::new(AtomicBool::new(false));
  let saw_after_flush = Arc::new(AtomicBool::new(false));
  let hooks = ApplyDeletesOnFlushHooks {
    docs_in_segment: docs_in_segment.clone(),
    closing: closing.clone(),
    saw_after_flush: saw_after_flush.clone(),
  };
  let mock = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  iwc
    .set_ram_buffer_size_mb(0.5)
    .set_max_buffered_docs(DISABLE_AUTO_FLUSH)
    .set_merge_policy(NoMergePolicy::default())
    .set_reader_pooling(false)
    .set_use_compound_file(true);
  let writer =
    IndexWriter::with_hooks(dir.clone(), iwc, Some(IndexWriterHooksEnum::custom(hooks)))?;

  let mut id = 0;
  let mut field_types = HashMap::new();
  loop {
    let mut body = String::new();
    for _ in 0..100 {
      body.push(' ');
      body.push_str(&TestUtil::random_realistic_unicode_string(&mut random));
    }
    if id == 500 {
      writer.delete_documents_with_terms(vec![Term::from_text("id", "0")])?;
    }
    let mut doc = Document::new();
    doc.add(new_string_field(
      &mut random,
      "id",
      id.to_string(),
      Store::No,
      &mut field_types,
    )?);
    doc.add(new_text_field(
      &mut random,
      "body",
      body,
      Store::No,
      &mut field_types,
    )?);
    writer.update_document_with_term(Term::from_text("id", id.to_string()), doc)?;
    docs_in_segment.fetch_add(1, Ordering::SeqCst);
    if slow_file_exists(dir.as_ref(), "_0_1.del")? || slow_file_exists(dir.as_ref(), "_0_1.liv")? {
      break;
    }
    id += 1;
  }
  closing.store(true, Ordering::SeqCst);
  assert!(saw_after_flush.load(Ordering::SeqCst));
  writer.close()?;

  Ok(())
}

#[test]
fn test_deletes_check_index_output() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  iwc
    .set_merge_policy(crate::core::index::no_merge_policy::NoMergePolicy::default())
    .set_max_buffered_docs(2);
  let w = IndexWriter::new(dir.clone(), iwc)?;
  let mut doc = Document::new();
  doc.add(StringField::from_string("field", "0", Store::No)?);
  w.add_document(doc)?;

  let mut doc = Document::new();
  doc.add(StringField::from_string("field", "1", Store::No)?);
  w.add_document(doc)?;
  w.commit()?;
  assert_eq!(1, w.get_segment_count());

  w.delete_documents_with_terms(vec![Term::from_text("field", "0")])?;
  w.commit()?;
  assert_eq!(1, w.get_segment_count());
  w.close()?;

  let mut output = Vec::with_capacity(1024);
  let mut checker = CheckIndex::<_, _, &mut Vec<u8>>::new(dir.clone())?;
  checker.set_info_stream(&mut output);
  let index_status = checker.check_index()?;
  assert!(index_status.clean);
  checker.close()?;
  drop(checker);
  let output = String::from_utf8_lossy(&output);

  // Segment should have deletions:
  assert!(output.contains("has deletions"));
  let analyzer = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;
  w.force_merge(1)?;
  w.close()?;

  let mut output = Vec::with_capacity(1024);
  let mut checker = CheckIndex::<_, _, &mut Vec<u8>>::new(dir.clone())?;
  checker.set_info_stream(&mut output);
  let index_status = checker.check_index()?;
  assert!(index_status.clean);
  checker.close()?;
  drop(checker);
  let output = String::from_utf8_lossy(&output);
  assert!(!output.contains("has deletions"));
  dir.as_ref().close()
}

#[test]
fn test_try_delete_document() -> Result<()> {
  use crate::core::index::directory_reader;
  use crate::core::index::directory_reader::DirectoryReader;
  use crate::core::index::multi_bits;
  use crate::core::index::no_merge_policy::NoMergePolicy;

  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let mock = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(mock)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;
  w.add_document(Document::new())?;
  w.add_document(Document::new())?;
  w.add_document(Document::new())?;
  w.close()?;

  let mock = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(mock)?;
  iwc.set_merge_policy(NoMergePolicy::default());
  let w = IndexWriter::new(dir.clone(), iwc)?;
  let r = directory_reader::open_from_writer(&w)?;
  assert_ne!(w.try_delete_document(&r, 1)?, -1);
  assert!(!r.is_current()?);

  let context = (&r).get_context()?;
  let leaves = context.leaves()?;
  let leaf_reader = leaves[0].reader();
  assert_ne!(w.try_delete_document(leaf_reader.clone(), 0)?, -1);
  assert!(!r.is_current()?);
  drop(r);
  w.close()?;

  let r = directory_reader::open(dir.clone())?;
  assert_eq!(2, r.num_deleted_docs()?);
  assert!(multi_bits::get_live_docs(&r)?.is_some());
  drop(r);

  Ok(())
}

#[test]
fn test_nrt_is_current_after_delete() -> Result<()> {
  use crate::core::index::directory_reader;
  use crate::core::index::directory_reader::DirectoryReader;
  use crate::core::index::term::Term;

  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let mock = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(mock)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;
  w.add_document(Document::new())?;
  w.add_document(Document::new())?;
  w.add_document(Document::new())?;
  w.add_document(Document::new())?;
  w.add_document(Document::new())?;
  let mut field_types = HashMap::new();
  let mut doc = Document::new();
  doc.add(new_string_field(
    &mut random,
    "id",
    "1",
    Store::Yes,
    &mut field_types,
  )?);
  w.add_document(doc)?;
  w.close()?;

  let mock = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(mock)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;
  let r = directory_reader::open_with_writer_deletes(&w, false, false)?;
  w.delete_documents_with_terms(vec![Term::from_text("id", "1")])?;
  let r2 = directory_reader::open_with_writer_deletes(&w, true, true)?;
  assert!(!r.is_current()?);
  assert!(r2.is_current()?);

  Ok(())
}

#[test]
fn test_only_deletes_triggers_merge_on_close() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(mock)?;
  iwc.set_max_buffered_docs(2);

  let mut mp = LogMergePolicy::log_doc();
  mp.set_min_merge_docs(1);
  iwc.set_merge_policy(mp);

  iwc.set_merge_scheduler(SerialMergeScheduler::new());

  let w = IndexWriter::new(dir.clone(), iwc)?;
  let mut field_types = HashMap::new();

  for i in 0..38 {
    let mut doc = Document::new();
    doc.add(new_string_field(
      &mut random,
      "id",
      i.to_string(),
      Store::No,
      &mut field_types,
    )?);
    w.add_document(doc)?;
  }
  w.commit()?;

  for i in 0..18 {
    w.delete_documents_with_terms(vec![Term::from_text("id", i.to_string())])?;
  }

  w.close()?;

  let r = directory_reader::open(dir.clone())?;
  let reader = r.get_context()?;
  assert_eq!(1, reader.leaves()?.len());
  Ok(())
}

#[test]
fn test_only_deletes_triggers_merge_on_get_reader() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(mock)?;
  iwc.set_max_buffered_docs(2);

  let mut mp = LogMergePolicy::log_doc();
  mp.set_min_merge_docs(1);
  iwc.set_merge_policy(mp);

  iwc.set_merge_scheduler(SerialMergeScheduler::new());

  let w = IndexWriter::new(dir.clone(), iwc)?;
  let mut field_types = HashMap::new();

  for i in 0..38 {
    let mut doc = Document::new();
    doc.add(new_string_field(
      &mut random,
      "id",
      i.to_string(),
      Store::No,
      &mut field_types,
    )?);
    w.add_document(doc)?;
  }
  w.commit()?;

  for i in 0..18 {
    w.delete_documents_with_terms(vec![Term::from_text("id", i.to_string())])?;
  }

  // First one triggers, but does not reflect, the merge:
  let _ = directory_reader::open_from_writer(&w)?;

  let r = directory_reader::open_from_writer(&w)?;
  let reader = r.get_context()?;
  assert_eq!(1, reader.leaves()?.len());

  w.close()?;
  Ok(())
}

#[test]
fn test_only_deletes_triggers_merge_on_flush() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(mock)?;
  iwc.set_max_buffered_docs(2);
  let mut mp = LogMergePolicy::log_doc();
  mp.set_min_merge_docs(1);
  iwc.set_merge_policy(mp);
  iwc.set_merge_scheduler(SerialMergeScheduler::new());

  let w = IndexWriter::new(dir.clone(), iwc)?;
  let mut field_types = HashMap::new();

  for i in 0..38 {
    let mut doc = Document::new();
    doc.add(new_string_field(
      &mut random,
      "id",
      i.to_string(),
      Store::No,
      &mut field_types,
    )?);
    w.add_document(doc)?;
  }
  w.commit()?;

  // Deleting 18 out of the 20 docs in the first segment make it the same "level" as the other 9
  // which should cause a merge to kick off:
  for i in 0..18 {
    w.delete_documents_with_terms(vec![Term::from_text("id", i.to_string())])?;
  }

  let _ = directory_reader::open_from_writer(&w)?;
  let reader = directory_reader::open_from_writer(&w)?;
  let reader = reader.get_context()?;
  assert_eq!(1, reader.leaves()?.len());
  w.close()?;
  Ok(())
}

#[test]
fn test_only_deletes_delete_all_docs() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(mock)?;
  iwc.set_max_buffered_docs(2);

  let mut mp = LogMergePolicy::log_doc();
  mp.set_min_merge_docs(1);
  iwc.set_merge_policy(mp);

  iwc.set_merge_scheduler(SerialMergeScheduler::new());

  let w = IndexWriter::new(dir.clone(), iwc)?;
  let mut field_types = HashMap::new();
  for i in 0..38 {
    let mut doc = Document::new();
    doc.add(new_string_field(
      &mut random,
      "id",
      i.to_string(),
      Store::No,
      &mut field_types,
    )?);
    w.add_document(doc)?;
  }
  w.commit()?;

  for i in 0..38 {
    w.delete_documents_with_terms(vec![Term::from_text("id", i.to_string())])?;
  }

  let r = directory_reader::open_from_writer(&w)?;
  assert_eq!(0, r.max_doc()?);
  let reader = r.get_context()?;
  assert_eq!(0, reader.leaves()?.len());
  w.close()?;
  Ok(())
}
#[test]
fn test_merging_after_delete_all() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let mock = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(mock)?;
  iwc.set_max_buffered_docs(2);

  let mut mp = LogMergePolicy::log_doc();
  mp.set_min_merge_docs(1);
  iwc.set_merge_policy(mp);
  iwc.set_merge_scheduler(SerialMergeScheduler::new());

  let writer = IndexWriter::new(dir.clone(), iwc)?;

  for i in 0..10 {
    let mut doc = Document::new();
    doc.add(StringField::from_string("id", i.to_string(), Store::No)?);
    writer.add_document(doc)?;
  }

  writer.commit()?;
  writer.delete_all()?;

  for i in 0..100 {
    let mut doc = Document::new();
    doc.add(StringField::from_string("id", i.to_string(), Store::No)?);
    writer.add_document(doc)?;
  }

  writer.force_merge(1)?;

  let reader = directory_reader::open_from_writer(&writer)?;
  assert_eq!(1, (&reader).get_context()?.leaves()?.len());
  reader.close()?;

  writer.close()?;

  Ok(())
}
