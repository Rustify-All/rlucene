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
use crate::core::codecs::codec::Codec;
use crate::core::codecs::segment_info_format::SegmentInfoFormat;
use crate::core::document::document::Document;
use crate::core::document::field::Store;
use crate::core::document::string_field::StringField;
use crate::core::document::text_field::TextField;
use crate::core::index::directory_reader;
use crate::core::index::index_file_names::IndexFileNames;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::no_merge_policy::NoMergePolicy;
use crate::core::index::segment_commit_info::SegmentCommitInfo;
use crate::core::index::segment_reader::SegmentReader;
use crate::core::index::term::Term;
use crate::core::store::directory::{DirEnum, Directory};
use crate::core::store::{DataInput, IOContext};
use crate::core::util::StringHelper;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::version::LATEST;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::util::lucene_test_case::{
  create_temp_dir, is_night_mode, new_directory_shared, new_fs_directory,
  new_index_writer_config_with_analyzer, random, random_from_seed,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::RngExt;
use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::Duration;

#[allow(dead_code)] // for quick
pub struct TestIndexWriterThreadsToSegments;

#[test]
fn test_segment_count_on_flush_basic() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let w = IndexWriter::new(dir, IndexWriterConfig::with_analyzer(analyzer)?)?;
  let starting_gun = Arc::new(Barrier::new(3));
  let start_done = Arc::new(Barrier::new(3));
  let middle_gun = Arc::new(Barrier::new(3));
  let final_gun = Arc::new(Barrier::new(2));

  thread::scope(|scope| -> Result<()> {
    let mut threads = Vec::new();
    for thread_id in 0..2 {
      let w = w.clone();
      let starting_gun = starting_gun.clone();
      let start_done = start_done.clone();
      let middle_gun = middle_gun.clone();
      let final_gun = final_gun.clone();
      threads.push(scope.spawn(move || -> Result<()> {
        starting_gun.wait();
        let mut doc = Document::new();
        doc.add(TextField::from_string(
          "field",
          "here is some text",
          Store::No,
        )?);
        w.add_document(doc.clone())?;
        start_done.wait();

        middle_gun.wait();
        if thread_id == 0 {
          w.add_document(doc)?;
        } else {
          final_gun.wait();
          w.add_document(doc)?;
        }
        Ok(())
      }));
    }

    starting_gun.wait();
    start_done.wait();

    let r = directory_reader::open_from_writer(&w)?;
    assert_eq!(2, r.num_docs()?);
    let num_segments = (&r).get_context()?.leaves()?.len();
    // 1 segment if the threads ran sequentially, else 2.
    assert!(num_segments <= 2);
    r.close()?;

    middle_gun.wait();
    threads.remove(0).join().expect("thread panicked")?;

    final_gun.wait();
    threads.remove(0).join().expect("thread panicked")?;

    let r = directory_reader::open_from_writer(&w)?;
    assert_eq!(4, r.num_docs()?);
    // Both threads should have shared a single thread state since they did not try to index
    // concurrently.
    assert_eq!(1 + num_segments, (&r).get_context()?.leaves()?.len());
    r.close()?;
    Ok(())
  })?;

  w.close()?;
  Ok(())
}

/** Maximum number of simultaneous threads to use for each iteration. */
const MAX_THREADS_AT_ONCE: usize = 10;

struct CheckSegmentCount {
  max_thread_count_per_iter: Arc<AtomicUsize>,
  indexing_count: Arc<AtomicUsize>,
  r: crate::core::index::standard_directory_reader::StandardDirectoryReader<DirEnum>,
}

impl CheckSegmentCount {
  fn new(
    w: &Arc<IndexWriter<DirEnum>>,
    max_thread_count_per_iter: Arc<AtomicUsize>,
    indexing_count: Arc<AtomicUsize>,
    random: &mut impl rand::Rng,
  ) -> Result<Self> {
    let r = directory_reader::open_from_writer(w)?;
    assert_eq!(0, (&r).get_context()?.leaves()?.len());
    let mut checker = CheckSegmentCount {
      max_thread_count_per_iter,
      indexing_count,
      r,
    };
    checker.set_next_iter_thread_count(random);
    Ok(checker)
  }

  fn run(&mut self, random: &mut impl rand::Rng) -> Result<()> {
    let old_segment_count = (&self.r).get_context()?.leaves()?.len();
    let r2 = directory_reader::open_if_changed(&self.r)?.unwrap();
    self.r.close()?;
    self.r = r2;
    let max_expected_segments =
      old_segment_count + self.max_thread_count_per_iter.load(Ordering::SeqCst);
    assert!((&self.r).get_context()?.leaves()?.len() <= max_expected_segments);
    self.set_next_iter_thread_count(random);
    Ok(())
  }

  fn set_next_iter_thread_count(&mut self, random: &mut impl rand::Rng) {
    self.indexing_count.store(0, Ordering::SeqCst);
    self.max_thread_count_per_iter.store(
      TestUtil::next_int(random, 1, MAX_THREADS_AT_ONCE as i32) as usize,
      Ordering::SeqCst,
    );
  }

  fn close(&self) -> Result<()> {
    self.r.close()
  }
}

#[test]
fn test_segment_count_on_flush_random() -> Result<()> {
  let mut random = random();
  let dir = new_fs_directory(&mut random, create_temp_dir()?)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;

  // Never trigger flushes (so we only flush on getReader).
  iwc.set_max_buffered_docs(100000000);
  iwc.set_ram_buffer_size_mb(-1.0);

  // Never trigger merges (so we can simplistically count flushed segments).
  iwc.set_merge_policy(NoMergePolicy::default());

  let w = IndexWriter::new(dir, iwc)?;

  // How many threads are indexing in the current cycle.
  let indexing_count = Arc::new(AtomicUsize::new(0));

  // How many threads we will use on each cycle.
  let max_thread_count = Arc::new(AtomicUsize::new(0));

  let checker = CheckSegmentCount::new(
    &w,
    max_thread_count.clone(),
    indexing_count.clone(),
    &mut random,
  )?;
  let checker = Arc::new(Mutex::new(checker));
  let random = Arc::new(Mutex::new(random));

  // We spin up 10 threads up front, but then in between flushes we limit how many can run on each
  // iteration.
  let iter = if is_night_mode() { 300 } else { 10 };

  // We use this to stop all threads once they've indexed their docs in the current iter, and pull
  // a new NRT reader, and verify the segment count.
  let barrier = Arc::new(Barrier::new(MAX_THREADS_AT_ONCE));

  thread::scope(|scope| -> Result<()> {
    let mut threads = Vec::new();
    for _ in 0..MAX_THREADS_AT_ONCE {
      let w = w.clone();
      let indexing_count = indexing_count.clone();
      let max_thread_count = max_thread_count.clone();
      let barrier = barrier.clone();
      let checker = checker.clone();
      let random = random.clone();
      threads.push(scope.spawn(move || -> Result<()> {
        for _ in 0..iter {
          if indexing_count.fetch_add(1, Ordering::SeqCst) < max_thread_count.load(Ordering::SeqCst)
          {
            let mut doc = Document::new();
            doc.add(TextField::from_string(
              "field",
              "here is some text that is a bit longer than normal trivial text",
              Store::No,
            )?);
            for _ in 0..200 {
              w.add_document(doc.clone())?;
            }
          }
          if barrier.wait().is_leader() {
            checker.lock().unwrap().run(&mut *random.lock().unwrap())?;
          }
          barrier.wait();
        }
        Ok(())
      }));
    }

    for handle in threads {
      handle.join().expect("thread panicked")?;
    }
    Ok(())
  })?;

  checker.lock().unwrap().close()?;
  w.close()?;
  Ok(())
}
#[test]
fn test_many_threads_close() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  iwc.set_commit_on_close(false);
  let writer = RandomIndexWriter::with_config(&mut random, dir, iwc);
  TestUtil::reduce_open_files(&writer.w)?;
  writer.set_do_random_force_merge(false);
  let w = Arc::new(writer);
  let num_threads = TestUtil::next_int(&mut random, 4, 30) as usize;
  let starting_gun = Arc::new(Barrier::new(num_threads + 1));

  thread::scope(|scope| -> Result<()> {
    let mut threads = Vec::new();
    for _ in 0..num_threads {
      let w = w.clone();
      let starting_gun = starting_gun.clone();
      let seed = random.random();
      threads.push(scope.spawn(move || -> Result<()> {
        let mut thread_random = random_from_seed(seed);
        starting_gun.wait();
        let mut doc = Document::new();
        doc.add(TextField::from_string(
          "field",
          "here is some text that is a bit longer than normal trivial text",
          Store::No,
        )?);
        for _ in 0..1000 {
          match w.add_document(&mut thread_random, doc.clone()) {
            Ok(_) => {},
            Err(LuceneError::AlreadyClosed(_)) => break,
            Err(e) => return Err(e),
          }
        }
        Ok(())
      }));
    }

    starting_gun.wait();
    thread::sleep(Duration::from_millis(100));
    if let Err(e) = w.close(&mut random)
      && !matches!(e, LuceneError::IllegalState(_))
    {
      return Err(e);
    }
    for handle in threads {
      handle.join().expect("thread panicked")?;
    }
    Ok(())
  })?;

  w.close(&mut random)?;
  Ok(())
}
#[test]
fn test_docs_stuck_in_ram_forever() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  iwc.set_ram_buffer_size_mb(0.2);
  let codec = TestUtil::get_default_codec();
  iwc.set_codec(codec.clone());
  iwc.set_merge_policy(NoMergePolicy::default());
  let w = IndexWriter::new(dir.clone(), iwc)?;
  let starting_gun = Arc::new(Barrier::new(3));

  thread::scope(|scope| -> Result<()> {
    let mut threads = Vec::new();
    for thread_id in 0..2 {
      let w = w.clone();
      let starting_gun = starting_gun.clone();
      threads.push(scope.spawn(move || -> Result<()> {
        starting_gun.wait();
        for _ in 0..10 {
          let mut doc = Document::new();
          doc.add(StringField::from_string(
            "field",
            format!("threadID{}", thread_id),
            Store::No,
          )?);
          w.add_document(doc)?;
        }
        Ok(())
      }));
    }

    starting_gun.wait();
    for handle in threads {
      handle.join().expect("thread panicked")?;
    }
    Ok(())
  })?;

  let mut seg_seen = HashSet::new();
  let mut thread0_count = 0;
  let mut thread1_count = 0;

  // At this point the writer should have 2 thread states w/ docs; now we index with only 1 thread
  // until we see all 1000 thread0 & thread1 docs flushed.
  let mut counter = 0;
  let mut check_at = 10;
  while thread0_count < 10 || thread1_count < 10 {
    let mut doc = Document::new();
    doc.add(StringField::from_string(
      "field",
      "threadIDmain",
      Store::No,
    )?);
    w.add_document(doc)?;
    if counter == check_at {
      for file_name in dir.list_all()? {
        if file_name.ends_with(".si") {
          let seg_name = IndexFileNames::parse_segment_name(&file_name);
          if !seg_seen.contains(seg_name) {
            seg_seen.insert(seg_name.to_string());
            let id = read_segment_info_id(&*dir, &file_name)?;
            let mut si = TestUtil::get_default_codec().segment_info_format().read(
              dir.clone(),
              seg_name,
              &id,
              &IOContext::default_io_context()?,
            )?;
            si.set_codec(codec.clone())?;
            let sci = SegmentCommitInfo::new(si, 0, 0, -1, -1, -1, Some(StringHelper::random_id()));
            let sr = SegmentReader::new(&sci, LATEST.major, &IOContext::default_io_context()?)?;
            thread0_count += LeafReader::doc_freq(&sr, &Term::from_text("field", "threadID0"))?;
            thread1_count += LeafReader::doc_freq(&sr, &Term::from_text("field", "threadID1"))?;
            sr.close()?;
          }
        }
      }

      check_at = ((check_at as f64) * 1.25) as usize;
      counter = 0;
    }
    counter += 1;
  }

  w.close()?;
  Ok(())
}
fn read_segment_info_id<D>(dir: &D, file: &str) -> Result<[u8; StringHelper::ID_LENGTH]>
where
  D: Directory,
{
  let mut input = dir.open_input(file, &IOContext::default_io_context()?)?;

  input.read_int()?; // magic
  input.read_string()?; // codec name
  input.read_int()?; // version

  let mut id = [0u8; StringHelper::ID_LENGTH];
  input.read_bytes(&mut id, 0, StringHelper::ID_LENGTH)?;

  Ok(id)
}
