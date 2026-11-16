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
use crate::core::document::int_point::IntPoint;
use crate::core::document::string_field::StringField;
use crate::core::document::text_field::TextField;
use crate::core::index::directory_reader;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_writer::DefaultIndexWriter;
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::merge_policy::MergePolicyEnum;
use crate::core::index::term::Term;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::store::ByteBuffersDirectory;
use crate::core::store::directory::Directory;
use crate::core::store::index_input::IndexInput;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::store::mock_directory_wrapper::MockDirectoryWrapper;
use crate::test_framework::core::util::english::English;
use crate::test_framework::core::util::lucene_test_case::{
  create_temp_dir_with_prefix, is_night_mode, new_fs_directory, random,
};
use rand_chacha::rand_core::Rng;
use std::sync::Arc;
use std::thread;

#[allow(dead_code)] // for quick search
struct TestAtomicUpdate;

impl TestAtomicUpdate {
  fn indexer_do_work<D>(writer: &DefaultIndexWriter<D>, current_iteration: i32) -> Result<()>
  where
    D: Directory + Send + Sync + 'static,
    D::IndexOutput: Send + Sync,
    D::IndexInput: Send + Sync,
    <D::IndexInput as IndexInput>::RandomAccessSlice: Send + Sync,
    D::Lock: Send + Sync,
  {
    // Update all 100 docs...
    for i in 0..100 {
      let mut d = Document::new();
      d.add(StringField::from_string("id", i.to_string(), Store::Yes)?);
      d.add(TextField::from_string(
        "contents",
        English::int_to_english(i + 10 * current_iteration),
        Store::No,
      )?);
      d.add(IntPoint::new("doc", [i])?);
      d.add(IntPoint::new("doc2d", [i, i])?);
      writer.update_document_with_term(Term::from_text("id", i.to_string()), d)?;
    }
    Ok(())
  }

  fn searcher_do_work<D>(directory: Arc<D>) -> Result<()>
  where
    D: Directory + Send + Sync + 'static,
    D::IndexInput: Send + Sync,
    <D::IndexInput as IndexInput>::RandomAccessSlice: Send + Sync,
  {
    let r = directory_reader::open(directory)?;
    assert_eq!(100, r.num_docs()?);
    r.close()
  }

  /*
   * Run N indexer and N searchers against single index as
   * stress test.
   */
  fn run_test<D, R>(random: &mut R, directory: Arc<D>) -> Result<()>
  where
    D: Directory + Send + Sync + 'static,
    D::IndexOutput: Send + Sync,
    D::IndexInput: Send + Sync,
    <D::IndexInput as IndexInput>::RandomAccessSlice: Send + Sync,
    D::Lock: Send + Sync,
    R: Rng + ?Sized,
  {
    let index_threads = if is_night_mode() { 2 } else { 1 };
    let search_threads = if is_night_mode() { 2 } else { 1 };
    let index_iterations = if is_night_mode() { 10 } else { 1 };
    let search_iterations = if is_night_mode() { 10 } else { 1 };

    let analyzer = MockAnalyzer::new(random);
    let mut conf = IndexWriterConfig::with_analyzer(analyzer)?;
    conf.set_max_buffered_docs(7);
    match &mut *conf.get_merge_policy_mut() {
      MergePolicyEnum::Tiered(merge_policy) => {
        merge_policy.set_max_merge_at_once(3)?;
      },
      _ => unreachable!(),
    }
    let writer = RandomIndexWriter::mock_index_writer(directory.clone(), conf, random)?;

    // Establish a base index of 100 docs:
    for i in 0..100 {
      let mut d = Document::new();
      d.add(StringField::from_string("id", i.to_string(), Store::Yes)?);
      d.add(TextField::from_string(
        "contents",
        English::int_to_english(i),
        Store::No,
      )?);
      if (i - 1) % 7 == 0 {
        writer.commit()?;
      }
      writer.add_document(d)?;
    }
    writer.commit()?;

    let r = directory_reader::open(directory.clone())?;
    assert_eq!(100, r.num_docs()?);
    r.close()?;

    let thread_results = thread::scope(|scope| {
      let mut handles = Vec::new();
      for _ in 0..index_threads {
        let writer = writer.clone();
        handles.push(scope.spawn(move || -> Result<()> {
          for count in 0..index_iterations {
            Self::indexer_do_work(&writer, count)?;
          }
          Ok(())
        }));
      }
      for _ in 0..search_threads {
        let directory = directory.clone();
        handles.push(scope.spawn(move || -> Result<()> {
          for _ in 0..search_iterations {
            Self::searcher_do_work(directory.clone())?;
          }
          Ok(())
        }));
      }

      let mut results = Vec::new();
      for handle in handles {
        results.push(handle.join());
      }
      results
    });

    writer.close()?;

    for thread_result in thread_results {
      match thread_result {
        Ok(Ok(())) => {},
        Ok(Err(err)) => return Err(err),
        Err(_) => return Err(LuceneError::illegal_state("hit error from thread")),
      }
    }

    Ok(())
  }
  fn test_atomic_updates() -> Result<()> {
    let mut random = random();

    // Run against a random directory.
    let directory = Arc::new(MockDirectoryWrapper::new(
      &mut random,
      ByteBuffersDirectory::new(),
    ));
    Self::run_test(&mut random, directory.clone())?;
    directory.close()?;

    // Then against an FSDirectory.
    let dir_path = create_temp_dir_with_prefix("lucene.test.atomic")?;
    let directory = new_fs_directory(&mut random, dir_path)?;
    Self::run_test(&mut random, directory.clone())?;
    directory.close()
  }
}

#[test]
fn test_atomic_updates() -> Result<()> {
  TestAtomicUpdate::test_atomic_updates()
}
