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
use crate::core::index::directory_reader::{self, DirectoryReader};
use crate::core::index::index_commit::IndexCommit;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::no_deletion_policy::NoDeletionPolicy;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, create_temp_dir_with_prefix, new_fs_directory, new_index_writer_config_with_analyzer,
  new_string_field, new_text_field, random, random_from_seed, slow_file_exists,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::RngExt;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

// Make sure if you use NoDeletionPolicy that no file
// referenced by a commit point is ever deleted

#[allow(dead_code)] // for quick search
pub struct TestNeverDelete;

#[test]
fn test_indexing() -> Result<()> {
  let mut random = random();
  let tmp_dir = create_temp_dir_with_prefix("TestNeverDelete")?;
  let d = new_fs_directory(&mut random, tmp_dir)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  iwc.set_index_deletion_policy(NoDeletionPolicy);
  let w = RandomIndexWriter::with_config(&mut random, d.clone(), iwc);
  w.w
    .get_config()?
    .set_max_buffered_docs(TestUtil::next_int(&mut random, 5, 30));
  let w = Arc::new(w);

  w.commit(&mut random)?;
  let mut index_threads = Vec::new();
  let index_thread_count = random.random_range(0..4);
  let stop_iterations = at_least(&mut random, 100);
  let field_types = Arc::new(Mutex::new(HashMap::new()));
  for x in 0..index_thread_count {
    let w = w.clone();
    let seed = random.random();
    let field_types = field_types.clone();
    index_threads.push(thread::Builder::new().name(format!("Thread {x}")).spawn(
      move || -> Result<()> {
        let mut random = random_from_seed(seed);
        let mut doc_count = 0;
        while doc_count < stop_iterations {
          let mut doc = Document::new();
          {
            let mut field_types = field_types.lock().unwrap();
            doc.add(new_string_field(
              &mut random,
              "dc",
              doc_count.to_string(),
              Store::Yes,
              &mut field_types,
            )?);
            doc.add(new_text_field(
              &mut random,
              "field",
              "here is some text",
              Store::Yes,
              &mut field_types,
            )?);
          }
          w.add_document(&mut random, doc)?;

          if doc_count % 13 == 0 {
            w.commit(&mut random)?;
          }
          doc_count += 1;
        }
        Ok(())
      },
    )?);
  }

  let mut all_files = HashSet::new();

  let mut r = directory_reader::open(d.clone())?;
  let mut iterations = 0;
  while {
    iterations += 1;
    iterations < stop_iterations
  } {
    let ic = r.get_index_commit()?;
    let file_names = ic.get_file_names()?;
    if cfg!(feature = "test_log_verbose") {
      println!("TEST: check files: {file_names:?}");
    }
    all_files.extend(file_names.iter().cloned());
    // Make sure no old files were removed
    for file_name in &all_files {
      assert!(
        slow_file_exists(&*d, file_name)?,
        "file {file_name} does not exist"
      );
    }
    if let Some(r2) = directory_reader::open_if_changed(&r)? {
      r.close()?;
      r = r2;
    }
    thread::sleep(Duration::from_millis(1));
  }
  r.close()?;

  for t in index_threads {
    t.join().expect("thread panicked")?;
  }
  w.close(&mut random)?;
  d.close()?;
  Ok(())
}
