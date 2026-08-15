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
use crate::core::document::field::Store::No;
use crate::core::index::directory_reader;
use crate::core::index::index_reader::Identity;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_writer::{IndexWriter, MAX_DOCS, set_max_docs};
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::multi_reader::MultiReader;
use crate::core::index::no_merge_policy::NoMergePolicy;
use crate::core::index::term::Term;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::search::index_searcher::{self, IndexSearcher};
use crate::core::search::sort::Sort;
use crate::core::search::sort_field::SortField;
use crate::core::search::sort_field::SortFieldType::Doc;
use crate::core::search::term_query::TermQuery;
use crate::core::search::top_docs::TopDocsLike;
use crate::core::search::top_score_doc_collector_manager::TopScoreDocCollectorManager;
use crate::core::store::directory::{DirEnum, Directory, MockDirWrapper};
#[cfg(feature = "nightly")]
use crate::core::store::lock::LockEnum2;
use crate::core::store::{IOContext, NoLockFactory};
use crate::core::util::HasIdentity;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::test_framework::core::util::lucene_test_case::{
  create_temp_dir_with_prefix, get_only_leaf_reader, new_directory_shared,
  new_directory_with_lock_factory, new_fs_directory, new_index_writer_config, new_mock_directory,
  new_string_field, random,
};
use crate::test_framework::core::util::test_util::TestUtil;
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::{Arc, Barrier};
use std::thread;

#[allow(dead_code)] // for quick search
struct TestIndexWriterMaxDocs;

struct AddIndexesFilterDirectory<D> {
  id: Identity,
  in_: Arc<D>,
}

impl<D> AddIndexesFilterDirectory<D>
where
  D: Directory,
{
  fn new(in_: Arc<D>) -> Self {
    Self {
      id: Identity::new(),
      in_,
    }
  }
}

impl<D> Display for AddIndexesFilterDirectory<D>
where
  D: Directory,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "FilterDirectory({})", self.in_)
  }
}

impl<D> CloseableRef for AddIndexesFilterDirectory<D> where D: Directory {}

impl<D> HasIdentity for AddIndexesFilterDirectory<D>
where
  D: Directory,
{
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl<D> Directory for AddIndexesFilterDirectory<D>
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

  fn copy_from<F>(&self, from: &F, src: &str, dest: &str, context: &IOContext) -> Result<()>
  where
    F: Directory + ?Sized,
  {
    self.in_.copy_from(from, src, dest, context)
  }

  fn get_pending_deletions(&self) -> Result<std::collections::HashSet<String>> {
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

#[cfg(feature = "nightly")]
type AddIndexesSourceDirectory = DirEnum;
#[cfg(feature = "nightly")]
type AddIndexesTargetDirectory = MockDirWrapper;
#[cfg(feature = "nightly")]
type AddIndexesSourceFilterDirectory = AddIndexesFilterDirectory<AddIndexesSourceDirectory>;

#[cfg(feature = "nightly")]
enum AddIndexesTestDirectory {
  Target(AddIndexesTargetDirectory),
  Filter(AddIndexesSourceFilterDirectory),
}

#[cfg(feature = "nightly")]
impl Display for AddIndexesTestDirectory {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Target(directory) => Display::fmt(directory, f),
      Self::Filter(directory) => Display::fmt(directory, f),
    }
  }
}

#[cfg(feature = "nightly")]
impl HasIdentity for AddIndexesTestDirectory {
  fn identity(&self) -> &Identity {
    match self {
      Self::Target(directory) => directory.identity(),
      Self::Filter(directory) => directory.identity(),
    }
  }
}

#[cfg(feature = "nightly")]
impl CloseableRef for AddIndexesTestDirectory {
  fn close(&self) -> Result<()> {
    match self {
      Self::Target(directory) => directory.close(),
      Self::Filter(directory) => directory.close(),
    }
  }
}

#[cfg(feature = "nightly")]
impl Directory for AddIndexesTestDirectory {
  fn list_all(&self) -> Result<Vec<String>> {
    match self {
      Self::Target(directory) => directory.list_all(),
      Self::Filter(directory) => directory.list_all(),
    }
  }

  fn delete_file(&self, name: &str) -> Result<()> {
    match self {
      Self::Target(directory) => directory.delete_file(name),
      Self::Filter(directory) => directory.delete_file(name),
    }
  }

  fn file_length(&self, name: &str) -> Result<usize> {
    match self {
      Self::Target(directory) => directory.file_length(name),
      Self::Filter(directory) => directory.file_length(name),
    }
  }

  type IndexOutput = <AddIndexesTargetDirectory as Directory>::IndexOutput;

  fn create_output(&self, name: &str, context: &IOContext) -> Result<Self::IndexOutput> {
    match self {
      Self::Target(directory) => directory.create_output(name, context),
      Self::Filter(directory) => directory.create_output(name, context),
    }
  }

  fn create_temp_output(
    &self,
    prefix: &str,
    suffix: &str,
    context: &IOContext,
  ) -> Result<Self::IndexOutput> {
    match self {
      Self::Target(directory) => directory.create_temp_output(prefix, suffix, context),
      Self::Filter(directory) => directory.create_temp_output(prefix, suffix, context),
    }
  }

  fn sync(&self, names: &[String]) -> Result<()> {
    match self {
      Self::Target(directory) => directory.sync(names),
      Self::Filter(directory) => directory.sync(names),
    }
  }

  fn sync_metadata(&self) -> Result<()> {
    match self {
      Self::Target(directory) => directory.sync_metadata(),
      Self::Filter(directory) => directory.sync_metadata(),
    }
  }

  fn rename(&self, source: &str, dest: &str) -> Result<()> {
    match self {
      Self::Target(directory) => directory.rename(source, dest),
      Self::Filter(directory) => directory.rename(source, dest),
    }
  }

  type IndexInput = <AddIndexesTargetDirectory as Directory>::IndexInput;

  fn open_input(&self, name: &str, context: &IOContext) -> Result<Self::IndexInput> {
    match self {
      Self::Target(directory) => directory.open_input(name, context),
      Self::Filter(directory) => directory.open_input(name, context),
    }
  }

  type Lock = LockEnum2<
    <AddIndexesTargetDirectory as Directory>::Lock,
    <AddIndexesSourceFilterDirectory as Directory>::Lock,
  >;

  fn obtain_lock(&self, name: &str) -> Result<Self::Lock> {
    match self {
      Self::Target(directory) => directory.obtain_lock(name).map(LockEnum2::A),
      Self::Filter(directory) => directory.obtain_lock(name).map(LockEnum2::B),
    }
  }

  fn copy_from<D>(&self, from: &D, src: &str, dest: &str, context: &IOContext) -> Result<()>
  where
    D: Directory + ?Sized,
  {
    match self {
      Self::Target(directory) => directory.copy_from(from, src, dest, context),
      Self::Filter(directory) => directory.copy_from(from, src, dest, context),
    }
  }

  fn get_pending_deletions(&self) -> Result<std::collections::HashSet<String>> {
    match self {
      Self::Target(directory) => directory.get_pending_deletions(),
      Self::Filter(directory) => directory.get_pending_deletions(),
    }
  }

  #[cfg(debug_assertions)]
  fn is_fs_directory(&self) -> bool {
    match self {
      Self::Target(directory) => directory.is_fs_directory(),
      Self::Filter(directory) => directory.is_fs_directory(),
    }
  }

  fn ensure_open(&self) -> Result<()> {
    match self {
      Self::Target(directory) => directory.ensure_open(),
      Self::Filter(directory) => directory.ensure_open(),
    }
  }
}

#[cfg(feature = "monster")]
#[test]
#[ignore = "monster"]
fn test_exactly_at_true_limit() -> Result<()> {
  let max_docs = MAX_DOCS;
  let mut random = random();

  let dir = new_fs_directory(&mut random, create_temp_dir_with_prefix("2BDocs3")?)?;

  let iwc = IndexWriterConfig::new()?;
  let iw = IndexWriter::new(dir.clone(), iwc)?;

  let mut field_types = HashMap::new();
  let mut doc = Document::new();
  doc.add(new_string_field(
    &mut random,
    "field",
    "text",
    No,
    &mut field_types,
  )?);

  for _i in 0..max_docs {
    iw.add_document(doc.clone())?;
  }

  iw.commit()?;

  // first unoptimized, then optimized
  for _iter in 0..2 {
    let ir = directory_reader::open(dir.clone())?;
    assert_eq!(max_docs, ir.max_doc()?);
    assert_eq!(max_docs, ir.num_docs()?);

    let searcher = index_searcher::from_reader(ir)?;
    let collector_manager = TopScoreDocCollectorManager::with_after(10, None, i32::MAX as usize)?;

    let hits = searcher.search_with_collector_manager(
      TermQuery::new(Term::from_text("field", "text")),
      &collector_manager,
    )?;
    assert_eq!(max_docs as usize, hits.total_hits.value);

    // sort by docID reversed
    let sort = Sort::with_fields(vec![SortField::with_reverse::<String>(None, Doc, true)?])?;
    let hits2 =
      searcher.search_with_sort(TermQuery::new(Term::from_text("field", "text")), 10, sort)?;

    assert_eq!(max_docs as usize, hits2.total_hits().value);
    assert_eq!(10, hits2.score_docs().len());
    assert_eq!(max_docs - 1, hits2.score_docs()[0].doc());

    iw.force_merge(1)?;
  }

  iw.close()?;
  Ok(())
}
#[test]
fn test_add_document() -> Result<()> {
  set_max_docs(10)?;
  let result = (|| -> Result<()> {
    let mut random = random();
    let dir = new_directory_shared(&mut random)?;
    let w = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;

    for _ in 0..10 {
      w.add_document(Document::new())?;
    }

    let err = w.add_document(Document::new());
    assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));

    w.close()?;
    Ok(())
  })();

  set_max_docs(MAX_DOCS)?;
  result
}
#[test]
fn test_add_documents() -> Result<()> {
  set_max_docs(10)?;
  let result = (|| -> Result<()> {
    let mut random = random();
    let dir = new_directory_shared(&mut random)?;
    let w = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;

    for _ in 0..10 {
      w.add_document(Document::new())?;
    }

    let err = w.add_documents(vec![Document::new()]);
    assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));

    w.close()?;
    Ok(())
  })();

  set_max_docs(MAX_DOCS)?;
  result
}
#[test]
fn test_update_document() -> Result<()> {
  set_max_docs(10)?;
  let result = (|| -> Result<()> {
    let mut random = random();
    let dir = new_directory_shared(&mut random)?;
    let w = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;

    for _ in 0..10 {
      w.add_document(Document::new())?;
    }

    let err = w.update_document_with_term(Term::from_text("field", "foo"), Document::new());
    assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));

    w.close()?;
    Ok(())
  })();

  set_max_docs(MAX_DOCS)?;
  result
}
#[test]
fn test_update_documents() -> Result<()> {
  set_max_docs(10)?;
  let result = (|| -> Result<()> {
    let mut random = random();
    let dir = new_directory_shared(&mut random)?;
    let w = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;

    for _ in 0..10 {
      w.add_document(Document::new())?;
    }

    let err = w.update_documents_with_term(Term::from_text("field", "foo"), vec![Document::new()]);
    assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));

    w.close()?;
    Ok(())
  })();

  set_max_docs(MAX_DOCS)?;
  result
}

#[test]
fn test_reclaimed_deletes() -> Result<()> {
  set_max_docs(10)?;
  let result = (|| -> Result<()> {
    let mut random = random();
    let dir = new_directory_shared(&mut random)?;
    let w = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;
    let mut field_types = HashMap::new();

    for i in 0..10 {
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

    for i in 0..5 {
      w.delete_documents_with_terms(vec![Term::from_text("id", i.to_string())])?;
    }

    w.force_merge(1)?;

    assert_eq!(5, w.get_doc_stats()?.max_doc);

    for _ in 0..5 {
      w.add_document(Document::new())?;
    }

    let err = w.add_document(Document::new());
    assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));

    w.close()?;
    Ok(())
  })();

  set_max_docs(MAX_DOCS)?;
  result
}
#[test]
fn test_reclaimed_deletes_whole_segments() -> Result<()> {
  set_max_docs(10)?;
  let result = (|| -> Result<()> {
    let mut random = random();
    let dir = new_directory_shared(&mut random)?;
    let mut iwc = IndexWriterConfig::new()?;
    iwc.set_merge_policy(NoMergePolicy::default());
    let w = IndexWriter::new(dir.clone(), iwc)?;
    let mut field_types = HashMap::new();

    for i in 0..10 {
      let mut doc = Document::new();
      doc.add(new_string_field(
        &mut random,
        "id",
        i.to_string(),
        Store::No,
        &mut field_types,
      )?);
      w.add_document(doc)?;
      if i % 2 == 0 {
        w.commit()?;
      }
    }

    for i in 0..5 {
      w.delete_documents_with_terms(vec![Term::from_text("id", i.to_string())])?;
    }

    w.force_merge(1)?;

    assert_eq!(5, w.get_doc_stats()?.max_doc);

    for _ in 0..5 {
      w.add_document(Document::new())?;
    }

    let err = w.add_document(Document::new());
    assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));

    w.close()?;
    Ok(())
  })();

  set_max_docs(MAX_DOCS)?;
  result
}

#[test]
fn test_add_indexes() -> Result<()> {
  set_max_docs(10)?;
  let result = (|| -> Result<()> {
    let mut random = random();
    let dir = new_directory_shared(&mut random)?;
    let w = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;

    for _ in 0..10 {
      w.add_document(Document::new())?;
    }
    w.close()?;
    drop(w);

    let dir2 = new_directory_shared(&mut random)?;
    let w2 = IndexWriter::new(dir2.clone(), IndexWriterConfig::new()?)?;
    w2.add_document(Document::new())?;

    let err = w2.add_indexes_from_directory(std::slice::from_ref(&dir));
    assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));

    assert_eq!(1, w2.get_doc_stats()?.max_doc);

    let ir = directory_reader::open(dir.clone())?;
    let err = TestUtil::add_indexes_slowly(&w2, &[&ir]);
    assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));

    w2.close()?;
    ir.close()?;
    Ok(())
  })();

  set_max_docs(MAX_DOCS)?;
  result
}
#[test]
fn test_multi_reader_exact_limit() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let w = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;
  for _ in 0..100000 {
    w.add_document(Document::new())?;
  }
  w.close()?;

  let remainder = MAX_DOCS % 100000;
  let dir2 = new_directory_shared(&mut random)?;
  let w = IndexWriter::new(dir2.clone(), IndexWriterConfig::new()?)?;
  for _ in 0..remainder {
    w.add_document(Document::new())?;
  }
  w.close()?;

  let copies = MAX_DOCS / 100000;

  let ir = Arc::new(directory_reader::open(dir.clone())?);
  let ir2 = Arc::new(directory_reader::open(dir2.clone())?);

  let mut sub_readers = vec![ir.clone(); copies as usize + 1];
  sub_readers[copies as usize] = ir2;

  let mr = MultiReader::new(sub_readers)?;
  assert_eq!(MAX_DOCS, mr.max_doc()?);
  assert_eq!(MAX_DOCS, mr.num_docs()?);

  Ok(())
}

#[test]
fn test_multi_reader_beyond_limit() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let w = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;
  for _ in 0..100000 {
    w.add_document(Document::new())?;
  }
  w.close()?;

  let mut remainder = MAX_DOCS % 100000;
  remainder += 1;

  let dir2 = new_directory_shared(&mut random)?;
  let w = IndexWriter::new(dir2.clone(), IndexWriterConfig::new()?)?;
  for _ in 0..remainder {
    w.add_document(Document::new())?;
  }
  w.close()?;

  let copies = MAX_DOCS / 100000;

  let ir = Arc::new(directory_reader::open(dir.clone())?);
  let ir2 = Arc::new(directory_reader::open(dir2.clone())?);

  let mut sub_readers = vec![ir.clone(); copies as usize + 1];
  sub_readers[copies as usize] = ir2;

  let err = MultiReader::new(sub_readers);
  assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));

  Ok(())
}
/// LUCENE-6299: Test if addindexes(Dir[]) prevents exceeding max docs.
// TODO: can we use the setter to lower the amount of docs to be written here?
#[cfg(feature = "nightly")]
#[test]
#[ignore = "nightly"]
fn test_add_too_many_indexes_dir() -> Result<()> {
  let mut random = random();

  // we cheat and add the same one over again... IW wants a write lock on each
  let source = Arc::new(new_directory_with_lock_factory(&mut random, NoLockFactory)?);
  let w = IndexWriter::new(source.clone(), IndexWriterConfig::new()?)?;
  for _ in 0..100000 {
    w.add_document(Document::new())?;
  }
  w.force_merge(1)?;
  w.commit()?;
  w.close()?;

  // wrap this with disk full, so test fails faster and doesn't fill up real disks.
  let target = new_mock_directory(&mut random)?;
  let target_dir = Arc::new(AddIndexesTestDirectory::Target(target.clone()));
  let w = IndexWriter::new(target_dir, IndexWriterConfig::new()?)?;
  w.commit()?; // don't confuse checkindex
  target.set_max_size_in_bytes(target.size_in_bytes()? as i64 + 65536); // 64KB

  let dirs_len = 1 + (MAX_DOCS / 100000);
  let mut dirs = Vec::new();
  for _ in 0..dirs_len {
    // bypass iw check for duplicate dirs
    dirs.push(Arc::new(AddIndexesTestDirectory::Filter(
      AddIndexesSourceFilterDirectory::new(source.clone()),
    )));
  }

  match w.add_indexes_from_directory(&dirs) {
    Ok(_) => return Err(LuceneError::illegal_state("didn't get expected exception")),
    Err(LuceneError::IllegalArgument(_)) => {
      // pass
    },
    Err(fake_disk_full) if fake_disk_full.to_string().contains("fake disk full") => {
      let mut e = LuceneError::illegal_state(
        "test failed: IW checks aren't working and we are executing add_indexes",
      );
      e.add_suppressed(fake_disk_full);
      return Err(e);
    },
    Err(e) => return Err(e),
  }
  assert_eq!(0, w.get_doc_stats()?.max_doc);

  w.close()?;
  Ok(())
}

/// LUCENE-6299: Test if addindexes(CodecReader[]) prevents exceeding max docs.
#[test]
fn test_add_too_many_indexes_codec_reader() -> Result<()> {
  let mut random = random();

  let source = Arc::new(new_directory_with_lock_factory(&mut random, NoLockFactory)?);
  let w = IndexWriter::new(source.clone(), IndexWriterConfig::new()?)?;
  for _ in 0..100000 {
    w.add_document(Document::new())?;
  }
  w.force_merge(1)?;
  w.commit()?;
  w.close()?;

  // wrap this with disk full, so test fails faster and doesn't fill up real disks.
  let target = Arc::new(new_mock_directory(&mut random)?);
  let w = IndexWriter::new(target.clone(), IndexWriterConfig::new()?)?;
  w.commit()?; // don't confuse checkindex
  target.set_max_size_in_bytes(target.size_in_bytes()? as i64 + 65536); // 64KB
  let r = directory_reader::open(source.clone())?;
  let seg_reader = get_only_leaf_reader(&r)?;

  let readers_len = 1 + (MAX_DOCS / 100000);
  let readers = vec![seg_reader; readers_len as usize];

  match w.add_indexes_from_codec_readers(readers) {
    Ok(_) => return Err(LuceneError::illegal_state("didn't get expected exception")),
    Err(LuceneError::IllegalArgument(_)) => {
      // pass
    },
    Err(fake_disk_full) if fake_disk_full.to_string().contains("fake disk full") => {
      let mut e = LuceneError::illegal_state(
        "test failed: IW checks aren't working and we are executing add_indexes",
      );
      e.add_suppressed(fake_disk_full);
      return Err(e);
    },
    Err(e) => return Err(e),
  }

  r.close()?;
  w.close()?;
  source.close()?;
  target.close()?;
  Ok(())
}
#[test]
fn test_too_large_max_docs() {
  let err = set_max_docs(i32::MAX);
  assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
}

#[test]
fn test_delete_all() -> Result<()> {
  set_max_docs(1)?;
  let result = (|| -> Result<()> {
    let mut random = random();
    let dir = new_directory_shared(&mut random)?;
    let w = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;

    w.add_document(Document::new())?;

    let err = w.add_document(Document::new());
    assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));

    w.delete_all()?;

    w.add_document(Document::new())?;

    let err = w.add_document(Document::new());
    assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));

    w.close()?;
    Ok(())
  })();

  set_max_docs(MAX_DOCS)?;
  result
}
#[test]
fn test_delete_all_after_flush() -> Result<()> {
  set_max_docs(2)?;
  let result = (|| -> Result<()> {
    let mut random = random();
    let dir = new_directory_shared(&mut random)?;
    let w = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;

    w.add_document(Document::new())?;
    directory_reader::open_from_writer(&w)?.close()?;

    w.add_document(Document::new())?;

    let err = w.add_document(Document::new());
    assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));

    w.delete_all()?;

    w.add_document(Document::new())?;
    w.add_document(Document::new())?;

    let err = w.add_document(Document::new());
    assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));

    w.close()?;
    Ok(())
  })();

  set_max_docs(MAX_DOCS)?;
  result
}

#[test]
fn test_delete_all_after_commit() -> Result<()> {
  set_max_docs(2)?;
  let result = (|| -> Result<()> {
    let mut random = random();
    let dir = new_directory_shared(&mut random)?;
    let w = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;

    w.add_document(Document::new())?;
    w.commit()?;

    w.add_document(Document::new())?;

    let err = w.add_document(Document::new());
    assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));

    w.delete_all()?;

    w.add_document(Document::new())?;
    w.add_document(Document::new())?;

    let err = w.add_document(Document::new());
    assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));

    w.close()?;
    Ok(())
  })();

  set_max_docs(MAX_DOCS)?;
  result
}

#[test]
fn test_delete_all_multiple_threads() -> Result<()> {
  let mut random = random();
  let limit = TestUtil::next_int(&mut random, 2, 10);
  set_max_docs(limit)?;
  let result = (|| -> Result<()> {
    let dir = new_directory_shared(&mut random)?;
    let w = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;

    let starting_gun = Arc::new(Barrier::new(limit as usize + 1));
    thread::scope(|scope| -> Result<()> {
      let mut threads = Vec::new();
      for _ in 0..limit {
        let starting_gun = starting_gun.clone();
        let w = &w;
        threads.push(scope.spawn(move || -> Result<()> {
          set_max_docs(limit)?;
          starting_gun.wait();
          w.add_document(Document::new())?;
          Ok(())
        }));
      }

      starting_gun.wait();

      for thread in threads {
        thread.join().expect("thread panicked")?;
      }

      Ok(())
    })?;

    let err = w.add_document(Document::new());
    assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));

    w.delete_all()?;
    for _ in 0..limit {
      w.add_document(Document::new())?;
    }
    let err = w.add_document(Document::new());
    assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));

    w.close()?;
    Ok(())
  })();

  set_max_docs(MAX_DOCS)?;
  result
}

#[test]
fn test_delete_all_after_close() -> Result<()> {
  set_max_docs(2)?;
  let result = (|| -> Result<()> {
    let mut random = random();
    let dir = new_directory_shared(&mut random)?;
    let w = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;
    w.add_document(Document::new())?;
    w.close()?;
    drop(w);

    let w2 = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;
    w2.add_document(Document::new())?;
    let err = w2.add_document(Document::new());
    assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));

    w2.delete_all()?;
    w2.add_document(Document::new())?;
    w2.add_document(Document::new())?;
    let err = w2.add_document(Document::new());
    assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));

    w2.close()?;
    Ok(())
  })();

  set_max_docs(MAX_DOCS)?;
  result
}

#[test]
fn test_across_two_index_writers() -> Result<()> {
  set_max_docs(1)?;
  let result = (|| -> Result<()> {
    let mut random = random();
    let dir = new_directory_shared(&mut random)?;
    let w = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;
    w.add_document(Document::new())?;
    w.close()?;
    drop(w);

    let w2 = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;
    let err = w2.add_document(Document::new());
    assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));

    w2.close()?;
    Ok(())
  })();

  set_max_docs(MAX_DOCS)?;
  result
}

#[test]
fn test_corrupt_index_exception_too_large() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let w = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;
  w.add_document(Document::new())?;
  w.add_document(Document::new())?;
  w.close()?;

  set_max_docs(1)?;
  let result = {
    let err = directory_reader::open(dir.clone());
    assert!(matches!(err, Err(LuceneError::CorruptIndex(_))));
    Ok(())
  };

  set_max_docs(MAX_DOCS)?;
  result
}

#[test]
fn test_corrupt_index_exception_too_large_writer() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let w = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;
  w.add_document(Document::new())?;
  w.add_document(Document::new())?;
  w.close()?;
  drop(w);

  set_max_docs(1)?;
  let result = {
    let err = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?);
    assert!(matches!(err, Err(LuceneError::CorruptIndex(_))));
    Ok(())
  };

  set_max_docs(MAX_DOCS)?;
  result
}
