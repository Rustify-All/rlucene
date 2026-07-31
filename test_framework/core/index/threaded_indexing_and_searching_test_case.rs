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
use crate::core::index::composite_reader::CompositeReader;
use crate::core::index::composite_reader_context::CompositeReaderContext;
use crate::core::index::directory_reader::DirectoryReader;
use crate::core::index::index_reader::{
  CacheHelper, CacheKey, IndexReader, IndexReaderContextKind,
};
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::{
  IndexReaderWarmer, IndexReaderWarmerEnum, IndexWriter, MAX_TERM_LENGTH,
};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::merge_policy::MergePolicyEnum;
use crate::core::index::multi_terms;
use crate::core::index::segment_reader::DefaultLeafReader;
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::term::Term;
use crate::core::index::terms::Terms;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::phrase_query::PhraseQuery;
use crate::core::search::query::Query;
use crate::core::search::sort::Sort;
use crate::core::search::sort_field::{SortField, SortFieldType};
use crate::core::search::term_query::TermQuery;
use crate::core::store::directory::{Directory, MockDirWrapper};
use crate::core::util::bits::Bits;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::info_stream::InfoStreamEnum;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::util::fail_on_non_bulk_merges_info_stream::FailOnNonBulkMergesInfoStream;
use crate::test_framework::core::util::line_file_docs::LineFileDocs;
use crate::test_framework::core::util::lucene_test_case::{
  ensure_sane_iwc_on_nightly, is_night_mode, new_index_writer_config_with_analyzer,
  new_mock_fs_directory, new_searcher_with_wrap, new_string_field, new_text_field,
  random_from_seed, random_multiplier,
};
use crate::test_framework::core::util::test_util::TestUtil;
use parking_lot::{Mutex, RwLock};
use rand::prelude::StdRng;
use rand::{RngExt, SeedableRng};
use std::collections::{HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, Ordering};
use std::thread;
use std::time::Duration;

// TODO
//   - mix in forceMerge, addIndexes
//   - randomly mix in non-congruent docs

pub type ThreadedIndexSearcher<DR> = IndexSearcher<CompositeReaderContext<Arc<DR>>>;

pub struct ThreadedIndexingAndSearchingTestCaseState<D>
where
  D: Directory,
{
  pub failed: AtomicBool,
  pub add_count: AtomicI32,
  pub del_count: AtomicI32,
  pub pack_count: AtomicI32,
  directory: RwLock<Option<Arc<D>>>,
  writer: RwLock<Option<Arc<IndexWriter<D>>>>,
  pub assert_merged_segments_warmed: AtomicBool,
  warmed: Arc<Mutex<HashSet<CacheKey>>>,
}

impl<D> Default for ThreadedIndexingAndSearchingTestCaseState<D>
where
  D: Directory,
{
  fn default() -> Self {
    Self::new()
  }
}

impl<D> ThreadedIndexingAndSearchingTestCaseState<D>
where
  D: Directory,
{
  pub fn new() -> Self {
    Self {
      failed: AtomicBool::new(false),
      add_count: AtomicI32::new(0),
      del_count: AtomicI32::new(0),
      pack_count: AtomicI32::new(0),
      directory: RwLock::new(None),
      writer: RwLock::new(None),
      assert_merged_segments_warmed: AtomicBool::new(true),
      warmed: Arc::new(Mutex::new(HashSet::new())),
    }
  }

  pub fn directory(&self) -> Arc<D> {
    self
      .directory
      .read()
      .as_ref()
      .expect("test directory has not been initialized")
      .clone()
  }

  pub fn writer(&self) -> Arc<IndexWriter<D>> {
    self
      .writer
      .read()
      .as_ref()
      .expect("test IndexWriter has not been initialized")
      .clone()
  }
}

struct SubDocs {
  pack_id: String,
  sub_ids: Vec<String>,
  deleted: bool,
}

struct MergedSegmentWarmer<D>
where
  D: Directory,
{
  warmed: Arc<Mutex<HashSet<CacheKey>>>,
  marker: std::marker::PhantomData<fn() -> D>,
  seed: u64,
}

impl<D> IndexReaderWarmer<D> for MergedSegmentWarmer<D>
where
  D: Directory + 'static,
{
  fn warm(&self, reader: &DefaultLeafReader<D>) -> Result<()> {
    if let Some(cache_helper) = reader.get_core_cache_helper()? {
      self.warmed.lock().insert(cache_helper.get_key());
    }
    let max_doc = reader.max_doc()?;
    let live_docs = reader.get_live_docs()?;
    let mut sum = 0;
    let inc = std::cmp::max(1, max_doc / 50);
    let mut stored_fields = reader.stored_fields()?;
    for doc_id in (0..max_doc).step_by(inc as usize) {
      if live_docs
        .as_ref()
        .map(|bits| bits.get(doc_id as usize))
        .transpose()?
        .unwrap_or(true)
      {
        let document = stored_fields.document(doc_id)?;
        sum += document.get_fields().len();
      }
    }

    let mut random = random_from_seed(self.seed);
    let searcher = new_searcher_with_wrap(&mut random, reader.clone(), false)?;
    sum += searcher
      .search(TermQuery::new(Term::from_text("body", "united")), 10)?
      .total_hits
      .value();
    let _ = sum;
    Ok(())
  }
}

/// Utility trait that spawns multiple indexing and searching threads.
pub trait ThreadedIndexingAndSearchingTestCase: Sync
where
  Self::Directory: Directory + 'static,
  Self::Reader: DirectoryReader<Directory = Self::Directory, DirectoryReader = Self::Reader>
    + CompositeReader<LeafReader = DefaultLeafReader<Self::Directory>>
    + Send
    + Sync
    + 'static,
  <Self::Reader as IndexReader>::ContextKind: IndexReaderContextKind<Arc<Self::Reader>>,
  CompositeReaderContext<Arc<Self::Reader>>: Send + Sync + 'static,
{
  type Directory: Directory + 'static;
  type Reader: DirectoryReader<Directory = Self::Directory, DirectoryReader = Self::Reader>
    + CompositeReader<LeafReader = DefaultLeafReader<Self::Directory>>
    + Send
    + Sync
    + 'static;

  fn state(&self) -> &ThreadedIndexingAndSearchingTestCaseState<Self::Directory>;

  // Called per search.
  fn get_current_searcher(
    &self,
    random: &mut StdRng,
  ) -> Result<Arc<ThreadedIndexSearcher<Self::Reader>>>;

  fn get_final_searcher(
    &self,
    random: &mut StdRng,
  ) -> Result<Arc<ThreadedIndexSearcher<Self::Reader>>>;

  fn release_searcher(&self, _searcher: Arc<ThreadedIndexSearcher<Self::Reader>>) -> Result<()> {
    Ok(())
  }

  // Called once to run searching.
  fn do_searching(&self, random: &mut StdRng, max_iterations: i32) -> Result<()>;

  fn get_directory(&self, directory: Arc<Self::Directory>) -> Arc<Self::Directory> {
    directory
  }

  fn update_documents(&self, id: Term, docs: Vec<Document>) -> Result<()> {
    self.state().writer().update_documents_with_term(id, docs)?;
    Ok(())
  }

  fn add_documents(&self, _id: Term, docs: Vec<Document>) -> Result<()> {
    self.state().writer().add_documents(docs)?;
    Ok(())
  }

  fn add_document(&self, _id: Term, document: Document) -> Result<()> {
    self.state().writer().add_document(document)?;
    Ok(())
  }

  fn update_document(&self, term: Term, document: Document) -> Result<()> {
    self
      .state()
      .writer()
      .update_document_with_term(term, document)?;
    Ok(())
  }

  fn delete_documents(&self, term: Term) -> Result<()> {
    self
      .state()
      .writer()
      .delete_documents_with_terms(vec![term])?;
    Ok(())
  }

  fn do_after_indexing_thread_done(&self) -> Result<()> {
    Ok(())
  }

  fn run_search_threads(&self, random: &mut StdRng, max_iterations: i32) -> Result<()> {
    let num_threads = if is_night_mode() {
      TestUtil::next_int(random, 1, 5)
    } else {
      2
    };
    let total_hits = AtomicI64::new(0);
    // Silly starting guess.
    let total_term_count = AtomicI32::new(100);
    let seeds = (0..num_threads)
      .map(|_| random.random::<u64>())
      .collect::<Vec<_>>();
    thread::scope(|scope| -> Result<()> {
      let mut search_threads = Vec::new();
      for seed in seeds {
        let total_hits = &total_hits;
        let total_term_count = &total_term_count;
        search_threads.push(scope.spawn(move || -> Result<()> {
          let mut random = StdRng::seed_from_u64(seed);
          let result = catch_unwind(AssertUnwindSafe(|| -> Result<()> {
            for _ in 1..max_iterations {
              if self.state().failed.load(Ordering::SeqCst) {
                break;
              }
              let searcher = self.get_current_searcher(&mut random)?;
              let search_result = catch_unwind(AssertUnwindSafe(|| -> Result<()> {
                // Verify 1) IndexWriter correctly sets diagnostics, and 2) segment warming for
                // merged segments actually happens.
                for leaf in searcher.get_index_reader().get_context()?.leaves()? {
                  let segment_reader = leaf.reader();
                  let diagnostics = segment_reader.get_segment_info().info.get_diagnostics();
                  let source = diagnostics
                    .get("source")
                    .expect("segment diagnostics must have a source");
                  if source == "merge"
                    && self
                      .state()
                      .assert_merged_segments_warmed
                      .load(Ordering::Relaxed)
                  {
                    let cache_helper = segment_reader
                      .get_core_cache_helper()?
                      .expect("SegmentReader must have a core cache helper");
                    assert!(
                      self.state().warmed.lock().contains(&cache_helper.get_key()),
                      "sub reader was not warmed: diagnostics={diagnostics:?}, si={}",
                      segment_reader.get_segment_info()
                    );
                  }
                }
                if searcher.get_index_reader().num_docs()? > 0 {
                  self.smoke_test_searcher(searcher.as_ref())?;
                  let Some(terms) = multi_terms::get_terms(searcher.get_index_reader(), "body")?
                  else {
                    return Ok(());
                  };
                  let mut terms_enum = terms.iterator()?;
                  let mut seen_term_count = 0;
                  let (shift, trigger) = if total_term_count.load(Ordering::SeqCst) < 30 {
                    (0, 1)
                  } else {
                    let trigger = total_term_count.load(Ordering::SeqCst) / 30;
                    (random.random_range(0..trigger), trigger)
                  };
                  for _ in 1..max_iterations {
                    let Some(term) = terms_enum.next()? else {
                      total_term_count.store(seen_term_count, Ordering::SeqCst);
                      break;
                    };
                    seen_term_count += 1;
                    // Search 30 terms.
                    if (seen_term_count + shift) % trigger == 0 {
                      total_hits.fetch_add(
                        self.run_query(
                          searcher.as_ref(),
                          TermQuery::new(Term::new("body", term.as_ref().clone())).into(),
                        )?,
                        Ordering::SeqCst,
                      );
                    }
                  }
                }
                Ok(())
              }));
              self.release_searcher(searcher)?;
              match search_result {
                Ok(result) => result?,
                Err(payload) => resume_unwind(payload),
              }
            }
            Ok(())
          }));
          match result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => {
              self.state().failed.store(true, Ordering::SeqCst);
              Err(error)
            },
            Err(payload) => {
              self.state().failed.store(true, Ordering::SeqCst);
              resume_unwind(payload)
            },
          }
        }));
      }
      for search_thread in search_threads {
        let _ = search_thread.join();
      }
      Ok(())
    })?;
    let _ = total_hits.load(Ordering::SeqCst);
    Ok(())
  }

  fn do_after_writer(&self, _random: &mut StdRng, _search_threads: Option<usize>) -> Result<()> {
    Ok(())
  }

  fn do_close(&self) -> Result<()> {
    Ok(())
  }

  #[allow(private_bounds)]
  fn run_test(&self, random: &mut StdRng, test_name: &str) -> Result<()>
  where
    Self::Directory: From<MockDirWrapper>,
  {
    self.state().failed.store(false, Ordering::SeqCst);
    self.state().add_count.store(0, Ordering::SeqCst);
    self.state().del_count.store(0, Ordering::SeqCst);
    self.state().pack_count.store(0, Ordering::SeqCst);
    self.state().warmed.lock().clear();
    self
      .state()
      .assert_merged_segments_warmed
      .store(true, Ordering::Relaxed);

    let docs = LineFileDocs::new(random)?;
    let directory = Arc::new(Self::Directory::from(new_mock_fs_directory(
      random,
      tempfile::Builder::new().prefix(test_name).tempdir()?,
    )?));
    let directory = self.get_directory(directory);
    *self.state().directory.write() = Some(directory.clone());

    let mut analyzer = MockAnalyzer::new(random);
    analyzer.set_max_token_length(TestUtil::next_int(random, 1, MAX_TERM_LENGTH));
    let mut config = new_index_writer_config_with_analyzer(random, analyzer)?;
    config.set_commit_on_close(false);
    config.set_info_stream(InfoStreamEnum::from(FailOnNonBulkMergesInfoStream));
    if let MergePolicyEnum::MockRandom(merge_policy) = config.get_merge_policy_mut() {
      merge_policy.set_do_non_bulk_merges(false);
    }
    ensure_sane_iwc_on_nightly(&mut config)?;
    config.set_merged_segment_warmer(Some(IndexReaderWarmerEnum::custom(MergedSegmentWarmer {
      warmed: self.state().warmed.clone(),
      marker: std::marker::PhantomData,
      seed: random.random(),
    })));
    let writer = IndexWriter::new(directory.clone(), config)?;
    TestUtil::reduce_open_files(writer.as_ref())?;
    *self.state().writer.write() = Some(writer.clone());

    let search_threads = if random.random_bool(0.5) {
      None
    } else {
      Some(2)
    };
    self.do_after_writer(random, search_threads)?;

    let num_index_threads = TestUtil::next_int(random, 2, 4);
    let max_iterations = if is_night_mode() {
      200
    } else {
      10 * random_multiplier()
    };
    let docs = Arc::new(Mutex::new(docs));
    let deleted_ids = Arc::new(Mutex::new(HashSet::<String>::new()));
    let deleted_pack_ids = Arc::new(Mutex::new(HashSet::<String>::new()));
    let all_sub_docs = Arc::new(Mutex::new(Vec::<Arc<Mutex<SubDocs>>>::new()));
    let field_to_type = Arc::new(Mutex::new(HashMap::<String, FieldType>::new()));
    let index_seeds = (0..num_index_threads)
      .map(|_| random.random::<u64>())
      .collect::<Vec<_>>();
    let search_seed = random.random::<u64>();

    thread::scope(|scope| -> Result<()> {
      let mut index_threads = Vec::new();
      for seed in index_seeds {
        let docs = docs.clone();
        let deleted_ids = deleted_ids.clone();
        let deleted_pack_ids = deleted_pack_ids.clone();
        let all_sub_docs = all_sub_docs.clone();
        let field_to_type = field_to_type.clone();
        index_threads.push(scope.spawn(move || -> Result<()> {
          let mut random = StdRng::seed_from_u64(seed);
          let result = catch_unwind(AssertUnwindSafe(|| -> Result<()> {
            let mut to_delete_ids = Vec::<String>::new();
            let mut to_delete_sub_docs = Vec::<Arc<Mutex<SubDocs>>>::new();
            for _ in 1..max_iterations {
              if self.state().failed.load(Ordering::SeqCst) {
                break;
              }
              // Occasional longish pause if running nightly.
              if is_night_mode() && random.random_range(0..6) == 3 {
                thread::sleep(Duration::from_millis(
                  TestUtil::next_int(&mut random, 50, 500) as u64,
                ));
              }
              // Rate limit ingest rate.
              if random.random_range(0..7) == 5 {
                thread::sleep(Duration::from_millis(
                  TestUtil::next_int(&mut random, 1, 10) as u64,
                ));
              }

              let mut document = docs.lock().next_doc()?;
              // Maybe add a randomly named field.
              let added_field = if random.random_bool(0.5) {
                let field_name = format!("extra{}", random.random_range(0..40));
                let field = new_text_field(
                  &mut random,
                  field_name.clone(),
                  "a random field",
                  Store::Yes,
                  &mut field_to_type.lock(),
                )?;
                document.add(field.clone());
                Some((field_name, field))
              } else {
                None
              };

              if random.random_bool(0.5) {
                if random.random_bool(0.5) {
                  // Add/update document block:
                  let deleted_sub_docs =
                    if !to_delete_sub_docs.is_empty() && random.random_bool(0.5) {
                      Some(
                        to_delete_sub_docs.remove(random.random_range(0..to_delete_sub_docs.len())),
                      )
                    } else {
                      None
                    };
                  let pack_id = if let Some(sub_docs) = deleted_sub_docs.as_ref() {
                    let sub_docs = sub_docs.lock();
                    assert!(!sub_docs.deleted);
                    sub_docs.pack_id.clone()
                  } else {
                    self
                      .state()
                      .pack_count
                      .fetch_add(1, Ordering::SeqCst)
                      .to_string()
                  };
                  let pack_id_field = new_string_field(
                    &mut random,
                    "packID",
                    pack_id.clone(),
                    Store::Yes,
                    &mut field_to_type.lock(),
                  )?;
                  document.add(pack_id_field.clone());
                  let mut docs_list = vec![document.clone()];
                  let mut docs_ids = vec![
                    document
                      .get("docid")?
                      .expect("LineFileDocs document must have docid")
                      .into_owned(),
                  ];
                  let max_doc_count = TestUtil::next_int(&mut random, 1, 10) as usize;
                  while docs_list.len() < max_doc_count {
                    let mut next_document = docs.lock().next_doc()?;
                    next_document.add(pack_id_field.clone());
                    if let Some((_, field)) = added_field.as_ref() {
                      next_document.add(field.clone());
                    }
                    docs_ids.push(
                      next_document
                        .get("docid")?
                        .expect("LineFileDocs document must have docid")
                        .into_owned(),
                    );
                    docs_list.push(next_document);
                  }
                  self
                    .state()
                    .add_count
                    .fetch_add(docs_list.len() as i32, Ordering::SeqCst);
                  let sub_docs = Arc::new(Mutex::new(SubDocs {
                    pack_id: pack_id.clone(),
                    sub_ids: docs_ids,
                    deleted: false,
                  }));
                  all_sub_docs.lock().push(sub_docs.clone());
                  let pack_id_term = Term::from_text("packID", &pack_id);
                  if let Some(deleted_sub_docs) = deleted_sub_docs {
                    let mut deleted_sub_docs = deleted_sub_docs.lock();
                    deleted_sub_docs.deleted = true;
                    deleted_ids
                      .lock()
                      .extend(deleted_sub_docs.sub_ids.iter().cloned());
                    self
                      .state()
                      .del_count
                      .fetch_add(deleted_sub_docs.sub_ids.len() as i32, Ordering::SeqCst);
                    drop(deleted_sub_docs);
                    self.update_documents(pack_id_term, docs_list)?;
                  } else {
                    self.add_documents(pack_id_term, docs_list)?;
                  }
                  document.remove_field("packID");
                  if random.random_range(0..5) == 2 {
                    to_delete_sub_docs.push(sub_docs);
                  }
                } else {
                  // Add single document.
                  let document_id = document
                    .get("docid")?
                    .expect("LineFileDocs document must have docid")
                    .into_owned();
                  self.add_document(Term::from_text("docid", &document_id), document.clone())?;
                  self.state().add_count.fetch_add(1, Ordering::SeqCst);
                  if random.random_range(0..5) == 3 {
                    to_delete_ids.push(document_id);
                  }
                }
              } else {
                // Update a single document, but never reuse an ID, so the delete never actually
                // happens.
                let document_id = document
                  .get("docid")?
                  .expect("LineFileDocs document must have docid")
                  .into_owned();
                self.update_document(Term::from_text("docid", &document_id), document.clone())?;
                self.state().add_count.fetch_add(1, Ordering::SeqCst);
                if random.random_range(0..5) == 3 {
                  to_delete_ids.push(document_id);
                }
              }

              if random.random_range(0..30) == 17 {
                for id in &to_delete_ids {
                  self.delete_documents(Term::from_text("docid", id))?;
                }
                self
                  .state()
                  .del_count
                  .fetch_add(to_delete_ids.len() as i32, Ordering::SeqCst);
                deleted_ids.lock().extend(to_delete_ids.drain(..));

                for sub_docs in to_delete_sub_docs.drain(..) {
                  let mut sub_docs = sub_docs.lock();
                  assert!(!sub_docs.deleted);
                  deleted_pack_ids.lock().insert(sub_docs.pack_id.clone());
                  self.delete_documents(Term::from_text("packID", &sub_docs.pack_id))?;
                  sub_docs.deleted = true;
                  deleted_ids.lock().extend(sub_docs.sub_ids.iter().cloned());
                  self
                    .state()
                    .del_count
                    .fetch_add(sub_docs.sub_ids.len() as i32, Ordering::SeqCst);
                }
              }
              if let Some((field_name, _)) = added_field {
                document.remove_field(&field_name);
              }
            }
            self.do_after_indexing_thread_done()
          }));
          match result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => {
              self.state().failed.store(true, Ordering::SeqCst);
              Err(error)
            },
            Err(payload) => {
              self.state().failed.store(true, Ordering::SeqCst);
              resume_unwind(payload)
            },
          }
        }));
      }

      // Let the index build up a bit.
      thread::sleep(Duration::from_millis(100));
      let mut search_random = StdRng::seed_from_u64(search_seed);
      let search_result = self.do_searching(&mut search_random, max_iterations);
      for index_thread in index_threads {
        let _ = index_thread.join();
      }
      search_result
    })?;

    let searcher = self.get_final_searcher(random)?;
    assert!(!self.state().failed.load(Ordering::SeqCst));
    let mut do_fail = false;

    // Verify that deleted IDs are in fact deleted.
    for id in deleted_ids.lock().iter() {
      let hits = searcher.search(TermQuery::new(Term::from_text("docid", id)), 1)?;
      if hits.total_hits.value() != 0 {
        do_fail = true;
      }
    }

    // Verify that deleted pack IDs are in fact deleted.
    for id in deleted_pack_ids.lock().iter() {
      let hits = searcher.search(TermQuery::new(Term::from_text("packID", id)), 1)?;
      if hits.total_hits.value() != 0 {
        do_fail = true;
      }
    }

    // Verify that every group of sub-documents is still in document-ID order.
    for sub_docs in all_sub_docs.lock().iter() {
      let sub_docs = sub_docs.lock();
      let mut hits = searcher.search(
        TermQuery::new(Term::from_text("packID", &sub_docs.pack_id)),
        20,
      )?;
      let mut stored_fields = searcher.stored_fields()?;
      if !sub_docs.deleted {
        // We sort by relevance but the scores should be identical so sort falls back to by doc ID.
        if hits.total_hits.value() != sub_docs.sub_ids.len() {
          do_fail = true;
        } else {
          let mut last_doc_id = -1;
          let mut start_doc_id = -1;
          for score_doc in &hits.score_docs {
            let doc_id = score_doc.doc;
            if last_doc_id != -1 {
              assert_eq!(1 + last_doc_id, doc_id);
            } else {
              start_doc_id = doc_id;
            }
            last_doc_id = doc_id;
            let document = stored_fields.document(doc_id)?;
            assert_eq!(
              sub_docs.pack_id.as_str(),
              document
                .get("packID")?
                .expect("packID must be stored")
                .as_str()
            );
          }

          last_doc_id = start_doc_id - 1;
          for sub_id in &sub_docs.sub_ids {
            hits = searcher.search(TermQuery::new(Term::from_text("docid", sub_id)), 1)?;
            assert_eq!(1, hits.total_hits.value());
            let doc_id = hits.score_docs[0].doc;
            if last_doc_id != -1 {
              assert_eq!(1 + last_doc_id, doc_id);
            }
            last_doc_id = doc_id;
          }
        }
      } else {
        // The pack was deleted, so make sure its documents are deleted. We cannot verify that the
        // pack ID is deleted because it can be reused for an update.
        for sub_id in &sub_docs.sub_ids {
          assert_eq!(
            0,
            searcher
              .search(TermQuery::new(Term::from_text("docid", sub_id)), 1)?
              .total_hits
              .value()
          );
        }
      }
    }

    // Verify that all non-deleted documents are in fact not deleted.
    let end_id = docs
      .lock()
      .next_doc()?
      .get("docid")?
      .expect("LineFileDocs document must have docid")
      .parse::<i32>()?;
    docs.lock().close();
    for id in 0..end_id {
      let string_id = id.to_string();
      if !deleted_ids.lock().contains(&string_id) {
        let hits = searcher.search(TermQuery::new(Term::from_text("docid", &string_id)), 1)?;
        if hits.total_hits.value() != 1 {
          do_fail = true;
        }
      }
    }
    assert!(!do_fail);

    assert_eq!(
      self.state().add_count.load(Ordering::SeqCst) - self.state().del_count.load(Ordering::SeqCst),
      searcher.get_index_reader().num_docs()?
    );
    self.release_searcher(searcher)?;

    writer.commit()?;
    assert_eq!(
      self.state().add_count.load(Ordering::SeqCst) - self.state().del_count.load(Ordering::SeqCst),
      writer.get_doc_stats()?.num_docs
    );

    self.do_close()?;
    let commit_result = catch_unwind(AssertUnwindSafe(|| writer.commit()));
    writer.close()?;
    match commit_result {
      Ok(result) => {
        result?;
      },
      Err(payload) => resume_unwind(payload),
    }
    TestUtil::check_index(random, directory.as_ref())?;
    directory.close()?;
    Ok(())
  }

  fn run_query(&self, searcher: &ThreadedIndexSearcher<Self::Reader>, query: Query) -> Result<i64> {
    searcher.search(query.clone(), 10)?;
    let sort = Sort::with_fields(vec![SortField::new(
      Some("titleDV"),
      SortFieldType::String,
    )?])?;
    let hit_count = searcher
      .search_with_sort(query.clone(), 10, sort.clone())?
      .base
      .total_hits
      .value();
    let hit_count2 = searcher
      .search_with_sort(query, 10, sort)?
      .base
      .total_hits
      .value();
    assert_eq!(hit_count, hit_count2);
    Ok(hit_count as i64)
  }

  fn smoke_test_searcher(&self, searcher: &ThreadedIndexSearcher<Self::Reader>) -> Result<()> {
    self.run_query(
      searcher,
      TermQuery::new(Term::from_text("body", "united")).into(),
    )?;
    self.run_query(
      searcher,
      TermQuery::new(Term::from_text("titleTokenized", "states")).into(),
    )?;
    self.run_query(
      searcher,
      PhraseQuery::from_terms_no_slop("body", &["united", "states"])?.into(),
    )?;
    Ok(())
  }
}
