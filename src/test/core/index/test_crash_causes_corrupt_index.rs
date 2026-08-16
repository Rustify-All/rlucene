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
use crate::core::index::directory_reader;
use crate::core::index::index_reader::{Identity, IndexReader};
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::term::Term;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::search::term_query::TermQuery;
use crate::core::search::top_docs::TopDocsLike;
use crate::core::store::directory::{DirEnum, Directory};
use crate::core::store::{FSDirectories, IOContext, IndexOutput};
use crate::core::util::HasIdentity;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::util::lucene_test_case::{
  create_temp_dir_with_prefix, new_fs_directory_from_path, new_index_writer_config_with_analyzer,
  new_searcher_with_wrap, new_text_field, random, slow_file_exists,
};
use parking_lot::Mutex;
use rand::prelude::StdRng;
use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter};
use std::path::PathBuf;
use std::sync::Arc;

#[allow(dead_code)] // for quick search
struct TestCrashCausesCorruptIndex {
  path: PathBuf,
  field_to_type: HashMap<String, FieldType>,
}

impl TestCrashCausesCorruptIndex {
  fn new() -> Result<Self> {
    Ok(Self {
      path: create_temp_dir_with_prefix("testCrashCorruptsIndexing")?.keep(),
      field_to_type: HashMap::new(),
    })
  }

  /** LUCENE-3627: This test fails. */
  fn test_crash_corrupts_indexing(&mut self, random: &mut StdRng) -> Result<()> {
    self.index_and_crash_on_create_output_segments2(random)?;

    self.search_for_fleas(random, 2)?;

    self.index_after_restart(random)?;

    self.search_for_fleas(random, 3)
  }

  /**
   * Index 1 document and commit. Prepare for crashing. Index 1 more document, and upon commit,
   * creation of segments_2 will crash.
   */
  fn index_and_crash_on_create_output_segments2(&mut self, random: &mut StdRng) -> Result<()> {
    let real_directory = Arc::new(FSDirectories::open(self.path.clone())?);
    let crash_after_create_output = Arc::new(CrashAfterCreateOutput::new(real_directory));

    // NOTE: cannot use RandomIndexWriter because it
    // sometimes commits:
    let analyzer = MockAnalyzer::new(random);
    let index_writer = IndexWriter::new(
      crash_after_create_output.clone(),
      new_index_writer_config_with_analyzer(random, analyzer)?,
    )?;

    index_writer.add_document(self.get_document(random)?)?;
    // writes segments_1:
    index_writer.commit()?;

    crash_after_create_output.set_crash_after_create_output("pending_segments_2");
    index_writer.add_document(self.get_document(random)?)?;
    // tries to write segments_2 but hits fake exc:
    assert!(matches!(
      index_writer.commit(),
      Err(LuceneError::IllegalState(_))
    ));

    // writes segments_3
    index_writer.close()?;
    assert!(!slow_file_exists(
      crash_after_create_output.get_delegate().as_ref(),
      "segments_2"
    )?);
    crash_after_create_output.close()
  }

  /** Attempts to index another 1 document. */
  fn index_after_restart(&mut self, random: &mut StdRng) -> Result<()> {
    let real_directory = new_fs_directory_from_path(random, self.path.clone())?;

    // LUCENE-3627 (before the fix): this line fails because
    // it doesn't know what to do with the created but empty
    // segments_2 file
    let analyzer = MockAnalyzer::new(random);
    let index_writer = IndexWriter::new(
      real_directory.clone(),
      new_index_writer_config_with_analyzer(random, analyzer)?,
    )?;

    // currently the test fails above.
    // however, to test the fix, the following lines should pass as well.
    index_writer.add_document(self.get_document(random)?)?;
    index_writer.close()?;
    assert!(!slow_file_exists(real_directory.as_ref(), "segments_2")?);
    real_directory.close()
  }

  /** Run an example search. */
  fn search_for_fleas(&self, random: &mut StdRng, expected_total_hits: usize) -> Result<()> {
    let real_directory = new_fs_directory_from_path(random, self.path.clone())?;
    let index_reader = directory_reader::open(real_directory.clone())?;
    let index_searcher = new_searcher_with_wrap(random, index_reader, true)?;
    let top_docs =
      index_searcher.search(TermQuery::new(Term::from_text(TEXT_FIELD, "fleas")), 10)?;
    assert_eq!(expected_total_hits, top_docs.total_hits().value);
    index_searcher.get_index_reader().close()?;
    real_directory.close()
  }

  /** Gets a document with content "my dog has fleas". */
  fn get_document(&mut self, random: &mut StdRng) -> Result<Document> {
    let mut document = Document::new();
    document.add(new_text_field(
      random,
      TEXT_FIELD,
      "my dog has fleas",
      Store::No,
      &mut self.field_to_type,
    )?);
    Ok(document)
  }
}

const TEXT_FIELD: &str = "text";

/**
 * This test type provides direct access to "simulating" a crash right after
 * `real_directory.create_output(..)` has been called on a certain specified name.
 */
struct CrashAfterCreateOutput<D> {
  id: Identity,
  in_: Arc<D>,
  crash_after_create_output: Mutex<Option<String>>,
}

impl<D> CrashAfterCreateOutput<D>
where
  D: Directory,
{
  fn new(real_directory: Arc<D>) -> Self {
    Self {
      id: Identity::new(),
      in_: real_directory,
      crash_after_create_output: Mutex::new(None),
    }
  }

  fn set_crash_after_create_output(&self, name: &str) {
    *self.crash_after_create_output.lock() = Some(name.to_string());
  }

  fn get_delegate(&self) -> &Arc<D> {
    &self.in_
  }
}

impl<D> Display for CrashAfterCreateOutput<D>
where
  D: Directory,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "CrashAfterCreateOutput({})", self.in_)
  }
}

impl<D> CloseableRef for CrashAfterCreateOutput<D>
where
  D: Directory,
{
  fn close(&self) -> Result<()> {
    self.in_.close()
  }
}

impl<D> HasIdentity for CrashAfterCreateOutput<D>
where
  D: Directory,
{
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl<D> Directory for CrashAfterCreateOutput<D>
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
    let mut index_output = self.in_.create_output(name, context)?;
    if self
      .crash_after_create_output
      .lock()
      .as_deref()
      .is_some_and(|crash_name| name == crash_name)
    {
      // CRASH!
      index_output.close()?;
      return Err(LuceneError::illegal_state(format!(
        "crashAfterCreateOutput {name}"
      )));
    }
    Ok(index_output)
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

#[test]
fn test_crash_corrupts_indexing() -> Result<()> {
  let mut random = random();
  TestCrashCausesCorruptIndex::new()?.test_crash_corrupts_indexing(&mut random)
}
