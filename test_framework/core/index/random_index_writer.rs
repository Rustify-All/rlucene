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
use crate::core::analysis::analyzer::AnalyzerEnum;
use crate::core::document::fields::Fields;
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::index::BytesRef;
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::directory_reader::{self, DirectoryReader};
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_writer::tests::INDEX_WRITER_ACCESS;
use crate::core::index::index_writer::{
  DefaultIndexWriter, DocStats, IndexCommitWrapper, IndexWriter, IndexWriterHooks,
  IndexWriterHooksEnum,
};
use crate::core::index::index_writer::{IndexingDocument, IntoFallibleIterator};
use crate::core::index::index_writer_config::{IndexWriterConfig, OpenMode};
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::merge_policy::{MergePolicyEnum, OneMerge};
use crate::core::index::standard_directory_reader::StandardDirectoryReader;
use crate::core::index::term::Term;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::search::query::Query;
use crate::core::search::term_query::TermQuery;
use crate::core::store::directory::Directory;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::info_stream::{InfoStream, InfoStreamEnum, InfoStreamMT};
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::internal::index_writer_access::IndexWriterAccess;
use crate::test_framework::core::util::lucene_test_case::{
  maybe_change_live_index_writer_config, new_index_writer_config_with_analyzer, random_from_seed,
};
use crate::test_framework::core::util::null_info_stream::NullInfoStream;
use crate::test_framework::core::util::test_util::TestUtil;
use parking_lot::Mutex;
use rand::prelude::StdRng;
use rand::{Rng, RngExt, SeedableRng};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
/// randomizes the indexing experience.
/// EG it may swap in a different merge policy/scheduler;
/// may commit periodically; may or may not forceMerge in the end,
/// may flush by doc count instead of RAM, etc.
pub struct RandomIndexWriter<D>
where
  D: Directory + 'static,
{
  pub w: DefaultIndexWriter<D>,
  flush_state: Mutex<FlushState>,
  get_reader_called: AtomicBool,
  soft_deletes_ratio: f64,
  do_random_force_merge: AtomicBool,
  do_random_force_merge_assert: AtomicBool,
  seed: u64,
}

struct FlushState {
  doc_count: i32,
  flush_at: i32,
  flush_at_factor: f64,
}

impl<D> RandomIndexWriter<D>
where
  D: Directory + 'static,
{
  /// Returns an indexwriter that randomly mixes up thread scheduling (by yielding at test points).
  pub fn mock_index_writer<R>(
    dir: Arc<D>,
    conf: IndexWriterConfig<D>,
    r: &mut R,
  ) -> Result<DefaultIndexWriter<D>>
  where
    R: Rng + ?Sized,
    D: Directory,
  {
    // Randomly yields the current thread to vary scheduling.
    let random = StdRng::seed_from_u64(r.random());
    Self::mock_index_writer_with_test_point(r, dir, conf, YieldTestPoint::new(random))
  }

  /// Returns an indexwriter that enables the specified test point.
  pub fn mock_index_writer_with_test_point<R, TP>(
    r: &mut R,
    dir: Arc<D>,
    mut conf: IndexWriterConfig<D>,
    test_point: TP,
  ) -> Result<DefaultIndexWriter<D>>
  where
    R: Rng + ?Sized,
    D: Directory,
    TP: TestPoint + 'static,
  {
    let info_stream = conf.get_info_stream();
    conf.set_info_stream(InfoStreamEnum::Custom(Box::new(TestPointInfoStream::new(
      info_stream,
      test_point,
    ))));

    if r.random()
      && directory_reader::index_exists(dir.as_ref())?
      && *conf.get_open_mode() != OpenMode::Create
    {
      if cfg!(feature = "test_log_verbose") {
        println!("RIW: open writer from reader");
      }
      let reader = directory_reader::open_with_sorter(dir.clone(), None)?;
      let commit = reader.get_index_commit()?;
      let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        IndexCommitWrapper::new(Some(commit), Some(&reader)).and_then(|index_commit| {
          IndexWriter::with_index_commit_and_hook(
            dir,
            conf,
            Some(IndexWriterHooksEnum::custom(TestPointsIndexWriterHooks)),
            index_commit,
          )
        })
      }));
      let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| reader.close()));
      match result {
        Ok(Ok(writer)) => match close_result {
          Ok(Ok(())) => Ok(writer),
          Ok(Err(error)) => Err(error),
          Err(payload) => std::panic::resume_unwind(payload),
        },
        Ok(Err(error)) => Err(error),
        Err(payload) => std::panic::resume_unwind(payload),
      }
    } else {
      IndexWriter::with_hooks(
        dir,
        conf,
        Some(IndexWriterHooksEnum::custom(TestPointsIndexWriterHooks)),
      )
    }
  }

  /// Creates a RandomIndexWriter with the provided config.
  pub fn with_config<R>(r: &mut R, dir: Arc<D>, config: IndexWriterConfig<D>) -> Self
  where
    R: Rng + ?Sized,
    D: Directory,
  {
    let use_soft_deletes = r.random();
    Self::new_with_config(r, dir, config, false, use_soft_deletes)
  }

  /// Creates a RandomIndexWriter with the provided config.
  pub fn with_soft_deletes<R>(
    r: &mut R,
    dir: Arc<D>,
    config: IndexWriterConfig<D>,
    use_soft_deletes: bool,
  ) -> Self
  where
    R: Rng + ?Sized,
    D: Directory,
  {
    Self::new_with_config(r, dir, config, false, use_soft_deletes)
  }

  fn new_with_config<R>(
    r: &mut R,
    dir: Arc<D>,
    mut c: IndexWriterConfig<D>,
    _close_analyzer: bool,
    use_soft_deletes: bool,
  ) -> Self
  where
    R: Rng + ?Sized,
    D: Directory,
  {
    let soft_deletes_ratio = if use_soft_deletes {
      c.set_soft_deletes_field("___soft_deletes");
      1.0 + r.random_range(0..10) as f64
    } else {
      0.0
    };

    let can_randomly_force_merge = !matches!(c.get_merge_policy(), MergePolicyEnum::No(_));
    let w = Self::mock_index_writer(dir.clone(), c, r).expect("should not fail");
    let flush_at = TestUtil::next_int(r, 10, 1000);
    if cfg!(feature = "test_log_verbose") {
      println!("RIW dir={}", dir);
    }

    // Make sure we sometimes test indices that don't get any forced merges.
    let do_random_force_merge = can_randomly_force_merge && r.random();
    let seed = r.random();
    Self {
      w,
      flush_state: Mutex::new(FlushState {
        doc_count: 0,
        flush_at,
        flush_at_factor: 1.0,
      }),
      get_reader_called: AtomicBool::new(false),
      soft_deletes_ratio,
      do_random_force_merge: AtomicBool::new(do_random_force_merge),
      do_random_force_merge_assert: AtomicBool::new(false),
      seed,
    }
  }

  /// Adds a Document.
  ///
  /// See [`IndexWriter::add_document`].
  pub fn add_document<R, DF>(&self, r: &mut R, doc: DF) -> Result<i64>
  where
    R: Rng + ?Sized,
    DF: IndexingDocument,
  {
    self.add_document_with_similarity(
      r,
      doc,
      crate::core::search::index_searcher::get_default_similarity().as_ref(),
    )
  }
  pub fn add_document_with_similarity<R, DF, S>(
    &self,
    r: &mut R,
    doc: DF,
    similarity: &S,
  ) -> Result<i64>
  where
    S: crate::core::search::similarities_impl::similarities::Similarity,
    R: Rng + ?Sized,
    DF: IndexingDocument,
  {
    maybe_change_live_index_writer_config(r, self.w.get_config_mut())?;
    let seq_no = if r.random_range(0..5) == 3 {
      self.w.add_documents_with_similarity([doc], similarity)
    } else {
      self.w.add_document_with_similarity(doc, similarity)
    }?;

    self.maybe_flush_or_commit(r)?;

    Ok(seq_no)
  }

  fn maybe_flush_or_commit<R>(&self, r: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    maybe_change_live_index_writer_config(r, self.w.get_config_mut())?;

    let (do_flush_or_commit, doc_count) = {
      let mut state = self.flush_state.lock();
      let do_flush_or_commit = state.doc_count == state.flush_at;
      state.doc_count += 1;
      (do_flush_or_commit, state.doc_count)
    };

    if do_flush_or_commit {
      if r.random_bool(0.5) {
        self.flush_all_buffers_sequentially(r)?;
      } else if r.random_bool(0.5) {
        if cfg!(feature = "test_log_verbose") {
          println!(
            "RIW.add/updateDocument: now doing a flush at docCount={}",
            doc_count
          );
        }
        self.w.flush()?;
      } else {
        if cfg!(feature = "test_log_verbose") {
          println!(
            "RIW.add/updateDocument: now doing a commit at docCount={}",
            doc_count
          );
        }
        self.w.commit()?;
      }

      let mut state = self.flush_state.lock();
      let min = (state.flush_at_factor * 10.0) as i32;
      let max = (state.flush_at_factor * 1000.0) as i32;
      state.flush_at += TestUtil::next_int(r, min, max);
      if state.flush_at_factor < 2e6 {
        // gradually but exponentially increase time b/w flushes
        state.flush_at_factor *= 1.05;
      }
    }

    Ok(())
  }

  fn flush_all_buffers_sequentially<R>(&self, r: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let thread_pool_size = INDEX_WRITER_ACCESS.get_doc_writer_thread_pool_size(&self.w);
    let num_flushes = std::cmp::min(1, r.random_range(0..=thread_pool_size));
    self.flush_all_buffers_sequentially_with_count(num_flushes)
  }

  fn flush_all_buffers_sequentially_with_count(&self, num_flushes: usize) -> Result<()> {
    if cfg!(feature = "test_log_verbose") {
      let doc_count = self.flush_state.lock().doc_count;
      println!(
        "RIW.add/updateDocument: now flushing the largest writer at docCount={}",
        doc_count
      );
    }
    for _ in 0..num_flushes {
      if !self.w.flush_next_buffer()? {
        break; // stop once we didn't flush anything
      }
    }
    Ok(())
  }

  pub fn add_documents<R, DI>(&self, r: &mut R, docs: DI) -> Result<i64>
  where
    R: Rng + ?Sized,
    DI: IntoFallibleIterator<Item: IndexingDocument>,
  {
    self.add_documents_with_similarity(
      r,
      docs,
      crate::core::search::index_searcher::get_default_similarity().as_ref(),
    )
  }
  pub fn add_documents_with_similarity<R, DI, S>(
    &self,
    r: &mut R,
    docs: DI,
    similarity: &S,
  ) -> Result<i64>
  where
    S: crate::core::search::similarities_impl::similarities::Similarity,
    R: Rng + ?Sized,
    DI: IntoFallibleIterator<Item: IndexingDocument>,
  {
    maybe_change_live_index_writer_config(r, self.w.get_config_mut())?;
    let seq_no = self.w.add_documents_with_similarity(docs, similarity)?;
    self.maybe_flush_or_commit(r)?;
    Ok(seq_no)
  }

  pub fn update_documents_with_term<R, T, DI>(
    &self,
    r: &mut R,
    del_term: T,
    docs: DI,
  ) -> Result<i64>
  where
    R: Rng + ?Sized,
    T: Into<Term>,
    DI: IntoFallibleIterator<Item: IndexingDocument>,
  {
    self.update_documents_with_term_and_similarity(
      r,
      del_term,
      docs,
      crate::core::search::index_searcher::get_default_similarity().as_ref(),
    )
  }
  pub fn update_documents_with_term_and_similarity<R, T, DI, S>(
    &self,
    r: &mut R,
    del_term: T,
    docs: DI,
    similarity: &S,
  ) -> Result<i64>
  where
    S: crate::core::search::similarities_impl::similarities::Similarity,
    R: Rng + ?Sized,
    T: Into<Term>,
    DI: IntoFallibleIterator<Item: IndexingDocument>,
  {
    maybe_change_live_index_writer_config(r, self.w.get_config_mut())?;
    let del_term = del_term.into();
    let seq_no = if self.use_soft_deletes(r) {
      let soft_deletes_field = self
        .w
        .get_config()?
        .get_soft_deletes_field()
        .expect("soft deletes field is not configured")
        .clone();
      self.w.soft_update_documents_with_similarity(
        del_term,
        docs,
        vec![NumericDocValuesField::new(soft_deletes_field, 1).into()],
        similarity,
      )
    } else if r.random_range(0..10) < 3 {
      // 30% chance
      self.w.update_documents_with_query_and_similarity(
        Some(Query::from(TermQuery::new(del_term))),
        docs,
        similarity,
      )
    } else {
      self
        .w
        .update_documents_with_term_and_similarity(Some(del_term), docs, similarity)
    }?;
    self.maybe_flush_or_commit(r)?;
    Ok(seq_no)
  }

  fn use_soft_deletes<R>(&self, r: &mut R) -> bool
  where
    R: Rng + ?Sized,
  {
    r.random::<f64>() < self.soft_deletes_ratio
  }

  /// Updates a document.
  ///
  /// See [`IndexWriter::update_document_with_term`].
  pub fn update_document_with_term<R, T, DF>(&self, r: &mut R, del_term: T, doc: DF) -> Result<i64>
  where
    R: Rng + ?Sized,
    T: Into<Term>,
    DF: IndexingDocument,
  {
    self.update_document_with_term_and_similarity(
      r,
      del_term,
      doc,
      crate::core::search::index_searcher::get_default_similarity().as_ref(),
    )
  }
  pub fn update_document_with_term_and_similarity<R, T, DF, S>(
    &self,
    r: &mut R,
    del_term: T,
    doc: DF,
    similarity: &S,
  ) -> Result<i64>
  where
    S: crate::core::search::similarities_impl::similarities::Similarity,
    R: Rng + ?Sized,
    T: Into<Term>,
    DF: IndexingDocument,
  {
    maybe_change_live_index_writer_config(r, self.w.get_config_mut())?;
    let del_term = del_term.into();
    let seq_no = if self.use_soft_deletes(r) {
      let soft_deletes_field = self
        .w
        .get_config()?
        .get_soft_deletes_field()
        .expect("soft deletes field is not configured")
        .clone();
      if r.random_range(0..5) == 3 {
        self.w.soft_update_documents_with_similarity(
          del_term,
          [doc],
          vec![NumericDocValuesField::new(soft_deletes_field, 1).into()],
          similarity,
        )
      } else {
        self.w.soft_update_document_with_similarity(
          del_term,
          doc,
          vec![NumericDocValuesField::new(soft_deletes_field, 1).into()],
          similarity,
        )
      }
    } else if r.random_range(0..5) == 3 {
      self
        .w
        .update_documents_with_term_and_similarity(Some(del_term), [doc], similarity)
    } else {
      self
        .w
        .update_document_with_term_and_similarity(Some(del_term), doc, similarity)
    }?;
    self.maybe_flush_or_commit(r)?;

    Ok(seq_no)
  }

  pub fn add_indexes_from_dir<R>(&self, r: &mut R, dirs: &[Arc<D>]) -> Result<i64>
  where
    R: Rng + ?Sized,
  {
    maybe_change_live_index_writer_config(r, self.w.get_config_mut())?;
    self.w.add_indexes_from_directory(dirs)
  }

  pub fn add_indexes_from_codec_readers<R, CR>(&self, r: &mut R, readers: Vec<CR>) -> Result<i64>
  where
    R: Rng + ?Sized,
    CR: CodecReader + 'static,
    OneMerge<D, Arc<CR>>: Send + 'static,
  {
    maybe_change_live_index_writer_config(r, self.w.get_config_mut())?;
    self.w.add_indexes_from_codec_readers(readers)
  }

  pub fn update_numeric_doc_value<R, T, F>(
    &self,
    r: &mut R,
    term: T,
    field: F,
    value: i64,
  ) -> Result<i64>
  where
    R: Rng + ?Sized,
    T: Into<Arc<Term>>,
    F: Into<String>,
  {
    maybe_change_live_index_writer_config(r, self.w.get_config_mut())?;
    self.w.update_numeric_doc_value(term, field, value)
  }

  pub fn update_binary_doc_value<R, T, F>(
    &self,
    r: &mut R,
    term: T,
    field: F,
    value: BytesRef<Vec<u8>>,
  ) -> Result<i64>
  where
    R: Rng + ?Sized,
    T: Into<Arc<Term>>,
    F: Into<String>,
  {
    maybe_change_live_index_writer_config(r, self.w.get_config_mut())?;
    self.w.update_binary_doc_value(term, field, value)
  }

  #[allow(unused)]
  pub fn update_doc_values<R, T>(&self, r: &mut R, term: T, updates: Vec<Fields>) -> Result<i64>
  where
    R: Rng + ?Sized,
    T: Into<Arc<Term>>,
  {
    maybe_change_live_index_writer_config(r, self.w.get_config_mut())?;
    self.w.update_doc_values(term, updates)
  }

  pub fn delete_documents_with_terms<R>(&self, r: &mut R, terms: Vec<Term>) -> Result<i64>
  where
    R: Rng + ?Sized,
  {
    maybe_change_live_index_writer_config(r, self.w.get_config_mut())?;
    self.w.delete_documents_with_terms(terms)
  }

  pub fn delete_documents_with_queries<R>(&self, r: &mut R, queries: Vec<Query>) -> Result<i64>
  where
    R: Rng + ?Sized,
  {
    maybe_change_live_index_writer_config(r, self.w.get_config_mut())?;
    self.w.delete_documents_with_queries(queries)
  }

  pub fn commit<R>(&self, r: &mut R) -> Result<i64>
  where
    R: Rng + ?Sized,
    D: Sync,
    Self: Sync,
  {
    let flush_concurrently = r.random_range(0..10) == 0;
    self.commit_with_flush_concurrently(r, flush_concurrently)
  }

  pub fn commit_with_flush_concurrently<R>(
    &self,
    r: &mut R,
    flush_concurrently: bool,
  ) -> Result<i64>
  where
    R: Rng + ?Sized,
    D: Sync,
    Self: Sync,
  {
    maybe_change_live_index_writer_config(r, self.w.get_config_mut())?;
    if flush_concurrently {
      let thread_pool_size = INDEX_WRITER_ACCESS.get_doc_writer_thread_pool_size(&self.w);
      let num_flushes = std::cmp::min(1, r.random_range(0..=thread_pool_size));
      let mut commit_result = None;
      let mut commit_error = None;
      let flush_result = thread::scope(|scope| {
        let thread = scope.spawn(|| self.flush_all_buffers_sequentially_with_count(num_flushes));
        match self.w.commit() {
          Ok(seq_no) => {
            commit_result = Some(seq_no);
          },
          Err(err) => {
            commit_error = Some(err);
          },
        }
        thread.join()
      });
      if let Some(mut primary) = commit_error {
        match flush_result {
          Ok(Ok(())) => {},
          Ok(Err(err)) => primary.add_suppressed(err),
          Err(payload) => primary.add_suppressed(LuceneError::tragedy_from_panic(
            "panic while flushing buffers",
            payload.as_ref(),
          )),
        }
        return Err(primary);
      }
      return Ok(commit_result.expect("commit result should be set"));
    }
    self.w.commit()
  }

  pub fn get_doc_stats(&self) -> Result<DocStats> {
    self.w.get_doc_stats()
  }

  pub fn delete_all(&self) -> Result<i64> {
    self.w.delete_all()
  }

  pub fn get_reader<R>(&self, r: &mut R) -> Result<StandardDirectoryReader<D>>
  where
    R: Rng + ?Sized,
  {
    maybe_change_live_index_writer_config(r, self.w.get_config_mut())?;
    self.get_reader_with_options(r, true, false)
  }

  #[allow(unused)]
  pub fn force_merge_deletes_with_wait<R>(&self, r: &mut R, do_wait: bool) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    maybe_change_live_index_writer_config(r, self.w.get_config_mut())?;
    self.w.force_merge_deletes_with_wait(do_wait)
  }

  #[allow(unused)]
  pub fn force_merge_deletes<R>(&self, r: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    maybe_change_live_index_writer_config(r, self.w.get_config_mut())?;
    self.w.force_merge_deletes()
  }

  pub fn set_do_random_force_merge(&self, v: bool) {
    self.do_random_force_merge.store(v, Ordering::SeqCst);
  }

  pub fn set_do_random_force_merge_assert(&self, v: bool) {
    self.do_random_force_merge_assert.store(v, Ordering::SeqCst);
  }

  fn do_random_force_merge<R>(&self, r: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    if self.do_random_force_merge.load(Ordering::SeqCst) {
      let seg_count = INDEX_WRITER_ACCESS.get_segment_count(&self.w);
      if r.random() || seg_count == 0 {
        // full forceMerge
        if cfg!(feature = "test_log_verbose") {
          println!("RIW: doRandomForceMerge(1)");
        }
        self.w.force_merge(1)?;
      } else if r.random() {
        // partial forceMerge
        let limit = TestUtil::next_usize(r, 1, seg_count);
        if cfg!(feature = "test_log_verbose") {
          println!("RIW: doRandomForceMerge({})", limit);
        }
        self.w.force_merge(limit as i32)?;
        if limit == 1
          || !matches!(
            self.w.get_config()?.get_merge_policy(),
            MergePolicyEnum::Tiered(_)
          )
        {
          assert!(
            !self.do_random_force_merge_assert.load(Ordering::SeqCst)
              || INDEX_WRITER_ACCESS.get_segment_count(&self.w) <= limit,
            "limit={} actual={}",
            limit,
            INDEX_WRITER_ACCESS.get_segment_count(&self.w)
          );
        }
      } else {
        if cfg!(feature = "test_log_verbose") {
          println!("RIW: do random forceMergeDeletes()");
        }
        self.w.force_merge_deletes()?;
      }
    }
    Ok(())
  }

  pub fn get_reader_with_options<R>(
    &self,
    r: &mut R,
    apply_deletions: bool,
    write_all_deletes: bool,
  ) -> Result<StandardDirectoryReader<D>>
  where
    R: Rng + ?Sized,
  {
    maybe_change_live_index_writer_config(r, self.w.get_config_mut())?;
    self.get_reader_called.store(true, Ordering::SeqCst);
    if r.random_range(0..20) == 2 {
      self.do_random_force_merge(r)?;
    }
    if !apply_deletions || r.random() {
      // if we have soft deletes we can't open from a directory
      if cfg!(feature = "test_log_verbose") {
        println!("RIW.getReader: use NRT reader");
      }
      if r.random_range(0..5) == 1 {
        self.w.commit()?;
      }
      INDEX_WRITER_ACCESS.get_reader(&self.w, apply_deletions, write_all_deletes)
    } else {
      if cfg!(feature = "test_log_verbose") {
        println!("RIW.getReader: open new reader");
      }
      self.w.commit()?;
      // TODO SoftDeletesDirectoryReaderWrapper未实现，暂时统一走 NRT reader。
      INDEX_WRITER_ACCESS.get_reader(&self.w, apply_deletions, write_all_deletes)
    }
  }

  pub fn close<R>(&self, r: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let pre_close_result =
      std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        if !INDEX_WRITER_ACCESS.is_closed(&self.w) {
          maybe_change_live_index_writer_config(r, self.w.get_config_mut())?;
        }
        // if someone isn't using getReader() API, we want to be sure to
        // forceMerge since presumably they might open a reader on the dir.
        if !self.get_reader_called.load(Ordering::SeqCst)
          && r.random_range(0..8) == 2
          && !INDEX_WRITER_ACCESS.is_closed(&self.w)
        {
          self.do_random_force_merge(r)?;
          if !self.w.get_config()?.get_commit_on_close() {
            // index may have changed, must commit the changes, or otherwise they are discarded by the
            // call to close()
            self.w.commit()?;
          }
        }
        Ok(())
      }));

    let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.w.close()));
    match pre_close_result {
      Ok(Ok(())) => match close_result {
        Ok(result) => result,
        Err(payload) => std::panic::resume_unwind(payload),
      },
      Ok(Err(error)) => Err(error),
      Err(payload) => std::panic::resume_unwind(payload),
    }
  }

  /// Forces a forceMerge.
  ///
  /// NOTE: this should be avoided in tests unless absolutely necessary, as it will result in less
  /// test coverage.
  ///
  /// See [`IndexWriter::force_merge`].
  pub fn force_merge<R>(&self, r: &mut R, max_num_segments: i32) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    maybe_change_live_index_writer_config(r, self.w.get_config_mut())?;
    self.w.force_merge(max_num_segments)
  }
}
impl<D> Drop for RandomIndexWriter<D>
where
  D: Directory + 'static,
{
  fn drop(&mut self) {
    let mut r = random_from_seed(self.seed);
    let _ = self.close(&mut r);
  }
}

static TEST_POINT_COMPONENT: &str = "TP";

struct TestPointInfoStream {
  delegate: InfoStreamMT,
  test_point: Arc<dyn TestPoint>,
}

impl TestPointInfoStream {
  fn new<TP>(delegate: InfoStreamMT, test_point: TP) -> Self
  where
    TP: TestPoint + 'static,
  {
    let delegate = if matches!(delegate.as_ref(), InfoStreamEnum::NoOutput(_)) {
      Arc::new(NullInfoStream.into())
    } else {
      delegate
    };
    Self {
      delegate,
      test_point: Arc::new(test_point),
    }
  }
}

impl crate::core::util::close::CloseableRef for TestPointInfoStream {
  fn close(&self) -> Result<()> {
    crate::core::util::close::CloseableRef::close(self.delegate.as_ref())
  }
}

impl InfoStream for TestPointInfoStream {
  fn message(&self, component: &str, message: &str) -> Result<()> {
    if component == TEST_POINT_COMPONENT {
      self.test_point.apply(message)?;
    }
    if self.delegate.is_enabled(component) {
      self.delegate.message(component, message)?;
    }
    Ok(())
  }

  fn is_enabled(&self, component: &str) -> bool {
    component == TEST_POINT_COMPONENT || self.delegate.is_enabled(component)
  }
}

impl<D> RandomIndexWriter<D>
where
  D: Directory + 'static,
{
  /// Writes all in-memory segments to the Directory.
  pub fn flush(&self) -> Result<()> {
    self.w.flush()
  }
}

struct TestPointsIndexWriterHooks;

impl IndexWriterHooks for TestPointsIndexWriterHooks {
  fn is_enable_test_points(&self) -> bool {
    true
  }
}

struct YieldTestPoint {
  random: Mutex<StdRng>,
}

impl YieldTestPoint {
  fn new(random: StdRng) -> Self {
    Self {
      random: Mutex::new(random),
    }
  }
}

impl TestPoint for YieldTestPoint {
  fn apply(&self, _message: &str) -> Result<()> {
    if self.random.lock().random_range(0..4) == 2 {
      thread::yield_now();
    }
    Ok(())
  }
}

/// Simple trait that is executed for each `TP` [`InfoStream`] component message.
/// See also [`RandomIndexWriter::mock_index_writer_with_test_point`].
pub trait TestPoint: Send + Sync {
  fn apply(&self, message: &str) -> Result<()>;
}

impl<T> TestPoint for Arc<T>
where
  T: TestPoint + ?Sized,
{
  fn apply(&self, message: &str) -> Result<()> {
    self.as_ref().apply(message)
  }
}

impl<D: Directory> RandomIndexWriter<D> {
  /// Creates a random config using MockAnalyzer and BM25.
  pub fn new<R>(r: &mut R, dir: Arc<D>) -> Result<Self>
  where
    R: Rng + ?Sized,
    D: Directory,
  {
    let a = MockAnalyzer::new(r);
    let config = new_index_writer_config_with_analyzer(r, a)?;
    let use_soft_deletes = r.random();
    Ok(Self::new_with_config(
      r,
      dir,
      config,
      true,
      use_soft_deletes,
    ))
  }
  /// Creates a random config using the provided analyzer and BM25.
  pub fn with_analyzer<R, T>(r: &mut R, dir: Arc<D>, analyzer: T) -> Result<Self>
  where
    R: Rng + ?Sized,
    D: Directory,
    T: Into<AnalyzerEnum>,
  {
    let config = new_index_writer_config_with_analyzer(r, analyzer)?;
    Ok(Self::with_config(r, dir, config))
  }
}
