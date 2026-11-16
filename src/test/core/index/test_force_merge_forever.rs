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
use crate::core::index::concurrent_merge_scheduler::ConcurrentMergeScheduler;
use crate::core::index::directory_reader;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_writer::{
  IndexWriter, IndexWriterHooks, IndexWriterHooksEnum, MAX_TERM_LENGTH,
};
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::merge_policy::{MergePolicyEnum, MergeStat};
use crate::core::index::term::Term;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::util::line_file_docs::LineFileDocs;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, new_directory_shared, new_index_writer_config_with_analyzer, random, random_from_seed,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::RngExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::thread;
#[allow(dead_code)] // for quick search
struct TestForceMergeForever;

// Just counts how many merges are done
struct MyIndexWriter {
  merge_count: Arc<AtomicI32>,
  first: Arc<AtomicBool>,
}

impl MyIndexWriter {
  fn new() -> Self {
    Self {
      merge_count: Arc::new(AtomicI32::new(0)),
      first: Arc::new(AtomicBool::new(true)),
    }
  }
}

impl IndexWriterHooks for MyIndexWriter {
  fn do_before_merge(&self, merge: &MergeStat) -> Result<()> {
    if merge.max_num_segments() != -1
      && (self.first.load(Ordering::SeqCst) || merge.segments.len() == 1)
    {
      self.first.store(false, Ordering::SeqCst);
      self.merge_count.fetch_add(1, Ordering::SeqCst);
    }
    Ok(())
  }
}
#[test]
fn test() -> Result<()> {
  let mut random = random();
  let d = new_directory_shared(&mut random)?;
  let mut analyzer = MockAnalyzer::new(&mut random);
  analyzer.set_max_token_length(TestUtil::next_int(&mut random, 1, MAX_TERM_LENGTH));
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  // SerialMergeScheduler can cause this test to run indefinitely long:
  iwc.set_merge_scheduler(ConcurrentMergeScheduler::new());
  let my_index_writer = MyIndexWriter::new();
  let merge_count = my_index_writer.merge_count.clone();
  let w = IndexWriter::with_hooks(
    d.clone(),
    iwc,
    Some(IndexWriterHooksEnum::custom(my_index_writer)),
  )?;

  // Try to make an index that requires merging:
  w.get_config()?
    .set_max_buffered_docs(TestUtil::next_int(&mut random, 2, 11));
  let num_start_docs = at_least(&mut random, 20);
  let mut docs = LineFileDocs::new(&mut random)?;
  for _ in 0..num_start_docs {
    w.add_document(docs.next_doc()?)?;
  }
  let merge_at_once = 1 + w.clone_segment_infos()?.size();
  let mut merge_policy = w.get_config()?.get_merge_policy_mut();
  match &mut *merge_policy {
    MergePolicyEnum::Tiered(mp) => {
      mp.set_max_merge_at_once(merge_at_once as i32)?;
    },
    MergePolicyEnum::LogDoc(mp) => {
      mp.set_merge_factor(merge_at_once)?;
    },
    MergePolicyEnum::LogBytesSize(mp) => {
      mp.set_merge_factor(merge_at_once)?;
    },
    _ => {
      drop(merge_policy);
      w.close()?;
      docs.close();
      return Ok(());
    },
  }
  drop(merge_policy);

  let do_stop = AtomicBool::new(false);
  w.get_config()?.set_max_buffered_docs(2);
  let seed = random.random();

  thread::scope(|scope| -> Result<()> {
    let w_ref = &w;
    let do_stop_ref = &do_stop;
    let handle = scope.spawn(move || -> Result<()> {
      let mut thread_random = random_from_seed(seed);
      let mut docs = LineFileDocs::new(&mut thread_random)?;
      while !do_stop_ref.load(Ordering::SeqCst) {
        w_ref.update_document_with_term(
          Term::from_text(
            "docid",
            TestUtil::next_int(&mut thread_random, 0, num_start_docs - 1).to_string(),
          ),
          docs.next_doc()?,
        )?;
        // Force deletes to apply
        directory_reader::open_from_writer(w_ref)?.close()?;
      }
      docs.close();
      Ok(())
    });

    w.force_merge(1)?;
    do_stop.store(true, Ordering::SeqCst);
    handle.join().unwrap()?;
    Ok(())
  })?;

  assert!(
    merge_count.load(Ordering::SeqCst) <= 1,
    "merge count is {}",
    merge_count.load(Ordering::SeqCst)
  );
  w.close()?;
  docs.close();
  Ok(())
}
