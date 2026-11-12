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
use crate::core::codecs::Codec;
use crate::core::codecs::live_docs_format::LiveDocsFormat;
use crate::core::codecs::segment_info_format::SegmentInfoFormat;
use crate::core::document::fields::Fields;
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::index::buffered_updates::BufferedUpdates;
use crate::core::index::documents_writer::FlushNotifications;
use crate::core::index::documents_writer_delete_queue::{
  DeleteSlice, DocumentsWriterDeleteQueue, Node,
};
use crate::core::index::field_infos::Builder;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::frozen_buffered_updates::FrozenBufferedUpdates;
use crate::core::search::similarities_impl::similarities::Similarity;

use crate::core::index::index_writer::{
  IndexWriter, IndexWriterDir, IndexingDocument, IntoFallibleIterator, SOURCE_FLUSH,
  create_compound_file, get_actual_max_docs, set_diagnostics,
};
use crate::core::index::indexing_chain::{IndexingChain, ReservedField};
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::lockable_concurrent_approximate_priority_queue::Lock;
use crate::core::index::pending_soft_deletes::count_soft_deletes;
use crate::core::index::segment_commit_info::SegmentCommitInfo;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::sorter::{DocMap, DocMapImpl};
use crate::core::store::IOContext;
use crate::core::store::directory::Directory;
use crate::core::store::flush_info::FlushInfo;
use crate::core::store::tracking_directory_wrapper::TrackingDirectoryWrapper;
use crate::core::util::accountable::Accountable;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::bit_set::BitSet;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::{CaughtResult, CaughtResultExt, LuceneError, Result};
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::info_stream::{InfoStream, InfoStreamMT};
use crate::core::util::io_consumer::IOConsumer;
use crate::core::util::io_utils::IOUtils;
use crate::core::util::ram_usage_estimator::size_of_vec;
use crate::core::util::{LATEST, LUCENE_10_0_0, StringHelper, TryIntoInt};
#[cfg(test)]
use crate::test_framework::core::util::failure_context::{
  ExecutionMethod, ExecutionOwner, ExecutionScope,
};
use parking_lot::{Condvar, Mutex};
use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter};
use std::sync::atomic::Ordering::SeqCst;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, Ordering};
use std::sync::{Arc, OnceLock};
use std::{fmt, thread};

const INFO_VERBOSE: bool = false;

pub struct DocumentsWriterPerThread<D>
where
  D: Directory,
{
  pub(crate) directory: Arc<TrackingDirectoryWrapper<Arc<IndexWriterDir<D>>>>,
  indexing_chain: IndexingChain<Arc<TrackingDirectoryWrapper<Arc<IndexWriterDir<D>>>>>,
  pending_updates: BufferedUpdates,
  pub(crate) segment_info: SegmentInfo<D>,
  field_infos: Builder,
  info_stream: InfoStreamMT,
  delete_slice: Option<DeleteSlice>,
  pending_num_docs: Arc<AtomicI64>,
  enable_test_points: bool,
  delete_doc_ids: Vec<i32>,
  num_deleted_doc_ids: usize,
  index_major_version_created: i32,
  files_to_delete: HashSet<String>,
  aborting_exception: OnceLock<CaughtResult>,
  pub(crate) state: Arc<State>,
  parent_field: Option<Box<Fields>>,
}

pub(crate) struct State {
  cvar: Condvar,
  lock_state: Mutex<Option<(thread::ThreadId, i32)>>,
  pub(crate) flush_pending: OnceLock<bool>,
  pub(crate) last_committed_bytes_used: AtomicI64,
  pub(crate) num_docs_in_ram: AtomicI32,
  pub(crate) id: Arc<str>,
  pub(crate) delete_queue: Arc<DocumentsWriterDeleteQueue>,
  has_flushed: OnceLock<bool>,
  pub(crate) aborted: AtomicBool,
}
impl State {
  pub(crate) fn is_flush_pending(&self) -> bool {
    self.flush_pending.get().copied().unwrap_or(false)
  }
  pub(crate) fn set_flush_pending(&self) -> Result<()> {
    if self.flush_pending.set(true).is_err() {
      return Err(LuceneError::illegal_state("flush_pending has been set"));
    }
    Ok(())
  }
  /// Returns the last committed bytes for this DWPT. This method can be called without acquiring the DWPT’s lock.
  pub(crate) fn get_last_committed_bytes_used(&self) -> i64 {
    self.last_committed_bytes_used.load(Ordering::SeqCst)
  }
  pub(crate) fn has_flushed(&self) -> bool {
    self.has_flushed.get().copied().unwrap_or(false)
  }
  pub(crate) fn get_num_docs_in_ram(&self) -> i32 {
    self.num_docs_in_ram.load(SeqCst)
  }
  pub(crate) fn is_aborted(&self) -> bool {
    self.aborted.load(Ordering::SeqCst)
  }
  pub(crate) fn is_queue_advanced(&self) -> bool {
    self.delete_queue.is_advanced()
  }

  /// Returns true if the DWPT's lock is held by the current thread.
  pub(crate) fn is_held_by_current_thread(&self) -> bool {
    self
      .lock_state
      .lock()
      .as_ref()
      .is_some_and(|(owner, _)| *owner == thread::current().id())
  }
}

impl Lock for State {
  fn lock(&self) {
    let current = thread::current().id();
    let mut guard = self.lock_state.lock();
    while guard.as_ref().is_some_and(|(owner, _)| *owner != current) {
      self.cvar.wait(&mut guard);
    }
    match guard.as_mut() {
      Some((_, hold_count)) => {
        // Fail if the signed lock hold count overflows.
        assert!(*hold_count < i32::MAX, "Maximum lock count exceeded");
        *hold_count += 1;
      },
      None => *guard = Some((current, 1)),
    }
  }

  fn try_lock(&self) -> bool {
    let current = thread::current().id();
    let mut guard = self.lock_state.lock();
    match guard.as_mut() {
      Some((owner, hold_count)) if *owner == current => {
        // Fail if the signed lock hold count overflows.
        assert!(*hold_count < i32::MAX, "Maximum lock count exceeded");
        *hold_count += 1;
        true
      },
      Some(_) => false,
      None => {
        *guard = Some((current, 1));
        true
      },
    }
  }

  fn unlock(&self) -> Result<()> {
    let mut guard = self.lock_state.lock();
    let current = thread::current().id();
    match guard.as_mut() {
      Some((owner, hold_count)) if *owner == current => {
        *hold_count -= 1;
        if *hold_count == 0 {
          *guard = None;
          self.cvar.notify_one();
        }
        Ok(())
      },
      _ => Err(LuceneError::illegal_state(
        "DWPT lock is not held by the current thread",
      )),
    }
  }
}

impl<D> DocumentsWriterPerThread<D>
where
  D: Directory,
{
  fn on_aborting_exception(&mut self, throwable: CaughtResult) {
    debug_assert!(
      self.aborting_exception.get().is_none(),
      "aborting error has already been set"
    );
    debug_assert!(!matches!(&throwable, Ok(Ok(()))));
    let _ = self.aborting_exception.set(throwable);
  }
  #[allow(dead_code)]
  pub(crate) fn is_aborted(&self) -> bool {
    self.state.is_aborted()
  }
  pub(crate) fn abort(&mut self) -> Result<()> {
    #[cfg(test)]
    let _execution_scope = ExecutionScope::enter(
      ExecutionOwner::DocumentsWriterPerThread,
      ExecutionMethod::Abort,
    );
    self.state.aborted.store(true, Ordering::SeqCst);
    self.pending_num_docs.fetch_add(
      -(self.state.num_docs_in_ram.load(SeqCst) as i64),
      Ordering::SeqCst,
    );

    let abort_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      if self.info_stream.is_enabled("DWPT") {
        self.info_stream.message("DWPT", "now abort")?;
      }

      let indexing_chain_result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.indexing_chain.abort()));
      self.pending_updates.clear();
      unwrap_caught_result!(indexing_chain_result)
    }));
    if self.info_stream.is_enabled("DWPT") {
      self.info_stream.message("DWPT", "done abort")?;
    }
    unwrap_caught_result!(abort_result)
  }
  #[allow(clippy::too_many_arguments)]
  pub(crate) fn new<L, N: Into<Arc<str>>>(
    index_major_version_created: i32,
    segment_name: N,
    directory_orig: Arc<D>,
    directory: Arc<IndexWriterDir<D>>,
    index_writer_config: &L,
    delete_queue: Arc<DocumentsWriterDeleteQueue>,
    field_infos: Builder,
    pending_num_docs: Arc<AtomicI64>,
    enable_test_points: bool,
  ) -> Result<Self>
  where
    L: LiveIndexWriterConfig,
  {
    let info_stream = index_writer_config.get_info_stream();
    let tracking_dir = TrackingDirectoryWrapper::new(directory);
    let directory_wrapped = Arc::new(tracking_dir);
    let pending_updates = BufferedUpdates::new();
    let delete_slice = Some(delete_queue.new_slice());
    let random_id = StringHelper::random_id();
    let id = StringHelper::id_to_string(Some(&random_id));
    let id: Arc<str> = Arc::from(id);
    let segment_info = SegmentInfo::new(
      directory_orig,
      Some((*LATEST).clone()),
      Some((*LATEST).clone()),
      segment_name,
      -1,
      false,
      false,
      Some(index_writer_config.get_codec().clone()),
      HashMap::new(),
      random_id,
      HashMap::new(),
      index_writer_config.get_index_sort().cloned(),
    )?;

    if INFO_VERBOSE && info_stream.is_enabled("DWPT") {
      info_stream.message(
        "DWPT",
        &format!(
          "{} init seg={} delQueue={}",
          std::thread::current().name().unwrap_or(""),
          segment_info.name,
          delete_queue
        ),
      )?;
    }

    let mut indexing_chain = IndexingChain::new(
      index_major_version_created,
      &segment_info,
      directory_wrapped.clone(),
      index_writer_config,
    )?;

    let state = State {
      cvar: Condvar::new(),
      lock_state: Mutex::new(None),
      flush_pending: OnceLock::new(),
      last_committed_bytes_used: AtomicI64::new(0),
      num_docs_in_ram: AtomicI32::new(0),
      id,
      delete_queue,
      has_flushed: OnceLock::new(),
      aborted: AtomicBool::new(false),
    };
    let parent_field = if let Some(parent_field) = index_writer_config.get_parent_field() {
      let field = ReservedField::new(NumericDocValuesField::new(parent_field, -1)).into();
      indexing_chain.mark_as_reserved(&field)?;
      Some(Box::new(field))
    } else {
      None
    };

    Ok(DocumentsWriterPerThread {
      directory: directory_wrapped,
      indexing_chain,
      pending_updates,
      segment_info,
      field_infos,
      info_stream,
      delete_slice,
      pending_num_docs,
      enable_test_points,
      delete_doc_ids: Vec::new(),
      num_deleted_doc_ids: 0,
      index_major_version_created,
      files_to_delete: HashSet::new(),
      aborting_exception: OnceLock::new(),
      state: Arc::new(state),
      parent_field,
    })
  }

  pub(crate) fn test_point(&self, message: &str) -> Result<()> {
    if self.enable_test_points {
      debug_assert!(self.info_stream.is_enabled("TP"));
      self.info_stream.message("TP", message)?;
    }
    Ok(())
  }
  /// Anything that will add N docs to the index should reserve first to make sure it's allowed.
  fn reserve_one_doc(&self) -> Result<()> {
    let new_count = self
      .pending_num_docs
      .fetch_add(1, Ordering::SeqCst)
      .wrapping_add(1);

    let max = get_actual_max_docs() as i64;
    if new_count > max {
      self.pending_num_docs.fetch_sub(1, Ordering::SeqCst);
      return Err(LuceneError::illegal_argument(format!(
        "number of documents in the index cannot exceed {max}"
      )));
    }
    Ok(())
  }
  pub(crate) fn update_documents<DI, FN, S>(
    &mut self,
    docs: DI,
    delete_node: Option<Arc<Node>>,
    flush_notifications: &FN,
    num_docs_in_ram: &AtomicI32,
    writer: &IndexWriter<D>,
    similarity: &S,
  ) -> Result<i64>
  where
    S: Similarity,
    DI: IntoFallibleIterator<Item: IndexingDocument>,
    FN: FlushNotifications,
  {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<i64> {
      self.test_point("DocumentsWriterPerThread addDocuments start")?;
      debug_assert!(
        self.aborting_exception.get().is_none(),
        "DWPT encountered an aborting error but is still indexing"
      );

      if INFO_VERBOSE && self.info_stream.is_enabled("DWPT") {
        self.info_stream.message(
          "DWPT",
          &format!(
            "{} update delTerm={} docID={} seg={} ",
            thread::current().name().unwrap_or(""),
            match delete_node {
              Some(ref node) => node.to_string(),
              None => "none".to_string(),
            },
            self.state.num_docs_in_ram.load(SeqCst),
            self.segment_info.name
          ),
        )?;
      }

      let docs_in_ram_before = self.state.num_docs_in_ram.load(SeqCst);
      let mut all_docs_indexed = false;
      let index_writer_config = writer.get_config()?;
      let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<i64> {
        let mut docs_iter = docs.into_fallible_iter().peekable();
        while let Some(doc) = docs_iter.next() {
          let doc = doc?;
          let use_parent_field = if self.parent_field.is_some() {
            docs_iter.peek().is_none()
          } else {
            if self.segment_info.index_sort.is_some()
              && docs_iter.peek().is_some()
              && self.index_major_version_created >= LUCENE_10_0_0.major
            {
              return Err(LuceneError::illegal_argument(
                "a parent field must be set in order to use document blocks with index sorting; see IndexWriterConfig#setParentField",
              ));
            }
            false
          };

          self.reserve_one_doc()?;
          let doc_id = self.state.num_docs_in_ram.fetch_add(1, Ordering::SeqCst);
          let process_result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
              let parent_field = self
                .parent_field
                .as_deref_mut()
                .filter(|_| use_parent_field);
              self.indexing_chain.process_document(
                doc_id,
                doc,
                parent_field,
                &mut self.segment_info,
                &mut self.field_infos,
                index_writer_config,
                &self.aborting_exception,
                similarity,
              )
            }));
          num_docs_in_ram.fetch_add(1, Ordering::SeqCst);
          unwrap_caught_result!(process_result)?;
        }

        let num_docs = self.state.num_docs_in_ram.load(SeqCst) - docs_in_ram_before;
        if num_docs > 1 {
          self.segment_info.set_has_blocks();
        }

        all_docs_indexed = true;
        let written = self.finish_documents(delete_node, docs_in_ram_before)?;
        Ok(written)
      }));

      let finally_result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
          if !all_docs_indexed && !self.state.aborted.load(Ordering::SeqCst) {
            // the iterator threw an error that is not aborting
            // go and mark all docs from this block as deleted
            let to_delete = self.state.num_docs_in_ram.load(SeqCst) - docs_in_ram_before;
            self.delete_last_docs(to_delete)?;
          }
          Ok(())
        }));
      IOUtils::finally_caught_result(result, finally_result)
    }));
    self.maybe_abort("updateDocuments", flush_notifications, writer)?;
    unwrap_caught_result!(result)
  }

  fn finish_documents(&mut self, delete_node: Option<Arc<Node>>, doc_id_upto: i32) -> Result<i64> {
    // here we actually finish the document in two steps 1. push the delete into
    // the queue and update our slice. 2. increment the DWPT private document
    // id.
    //
    //the updated slice we get from 1. holds all the deletes that have occurred
    //since we updated the slice the last time.
    //
    // Apply delTerm only after all indexing has
    //succeeded, but apply it only to docs prior to when
    //this batch started:
    let delete_slice = self
      .delete_slice
      .as_mut()
      .ok_or_else(|| LuceneError::illegal_state("document writer delete slice is missing"))?;
    let seq_no: i64 = if let Some(node) = delete_node {
      let seq = self
        .state
        .delete_queue
        .add_with_slice(node.clone(), delete_slice)?;
      debug_assert!(
        delete_slice.is_tail(&node),
        "expected the delete term as the tail item"
      );
      delete_slice.apply(&mut self.pending_updates, doc_id_upto)?;
      seq
    } else {
      let mut seq = self.state.delete_queue.update_slice(delete_slice)?;
      if seq < 0 {
        seq = -seq;
        delete_slice.apply(&mut self.pending_updates, doc_id_upto)?;
      } else {
        delete_slice.reset();
      }
      seq
    };

    Ok(seq_no)
  }
  // This method marks the last N docs as deleted. This is used
  // in the case of a non-aborting error. There are several cases
  // where we fail a document ie. due to an error during analysis
  // that causes the doc to be rejected but won't cause the DWPT to be
  // stale nor the entire IW to abort and shutdown. In such a case
  // we only mark these docs as deleted and turn it into a livedocs
  // during flush
  fn delete_last_docs(&mut self, doc_count: i32) -> Result<()> {
    let from = self.state.num_docs_in_ram.load(SeqCst) - doc_count;
    let to = self.state.num_docs_in_ram.load(SeqCst);
    let new_len = self.num_deleted_doc_ids + (to - from) as usize;
    ArrayUtil::grow_capacity(&mut self.delete_doc_ids, new_len)?;

    for doc_id in from..to {
      self.delete_doc_ids.push(doc_id);
      self.num_deleted_doc_ids += 1;
    }
    // NOTE: we do not trigger flush here.  This is
    // potentially a RAM leak, if you have an app that tries
    // to add docs but every single doc always hits a
    // non-aborting error.  Allowing a flush here gets
    // very messy because we are only invoked when handling
    // errors so to do this properly, while handling an
    // error we'd have to go off and flush new deletes
    // which is risky (likely would hit some other
    // confounding error).
    Ok(())
  }
  /// Returns the number of RAM resident documents in this [`DocumentsWriterPerThread`]
  pub(crate) fn get_num_docs_in_ram(&self) -> i32 {
    self.state.get_num_docs_in_ram()
  }
  /// Prepares this DWPT for flushing. This method will freeze and return the [`DocumentsWriterDeleteQueue`]’s global buffer and apply all pending deletes to this DWPT.
  pub(crate) fn prepare_flush(&mut self) -> Result<Option<FrozenBufferedUpdates>> {
    debug_assert!(self.state.num_docs_in_ram.load(SeqCst) > 0);

    let global_updates = self
      .state
      .delete_queue
      .freeze_global_buffer(self.delete_slice.as_mut())?;
    // deleteSlice can possibly be None if we have hit non-aborting errors during indexing and never succeeded adding a document
    if let Some(delete_slice) = self.delete_slice.as_mut() {
      // apply all deletes before we flush and release the delete slice
      delete_slice.apply(
        &mut self.pending_updates,
        self.state.num_docs_in_ram.load(SeqCst),
      )?;
      debug_assert!(delete_slice.is_empty());
      delete_slice.reset();
    }
    Ok(global_updates)
  }
  ///  Flush all pending docs to a new segment
  pub(crate) fn flush<FN>(
    &mut self,
    flush_notifications: &FN,
    writer: &IndexWriter<D>,
  ) -> Result<Option<FlushedSegment<D>>>
  where
    FN: FlushNotifications,
  {
    #[cfg(test)]
    let _execution_scope = ExecutionScope::enter(
      ExecutionOwner::DocumentsWriterPerThread,
      ExecutionMethod::Flush,
    );
    debug_assert_eq!(self.state.flush_pending.get(), Some(&true));
    debug_assert!(self.state.num_docs_in_ram.load(SeqCst) > 0);
    debug_assert!(
      self.delete_slice.as_ref().is_none_or(|ds| ds.is_empty()),
      "all deletes must be applied in prepareFlush"
    );

    self
      .segment_info
      .set_max_doc(self.state.num_docs_in_ram.load(SeqCst))?;

    let index_writer_config = writer.get_config()?;
    let io_context = IOContext::with_flush(FlushInfo::new(
      self.state.num_docs_in_ram.load(SeqCst),
      self.state.last_committed_bytes_used.load(Ordering::SeqCst),
    ))?;
    let directory = Arc::clone(&self.directory);
    let mut flush_state = SegmentWriteState::new(
      self.info_stream.clone(),
      &directory,
      Arc::new(self.field_infos.finish_and_take_names()?),
      &io_context,
    );

    let start_mb_used =
      self.state.last_committed_bytes_used.load(Ordering::SeqCst) as f64 / 1024.0 / 1024.0;

    // Apply delete-by-docID now (delete-byDocID only
    // happens when an error is hit processing that
    // doc, eg if analyzer has some problem w/ the text):
    if self.num_deleted_doc_ids > 0 {
      let max_doc = self.state.num_docs_in_ram.load(SeqCst) as usize;
      let mut live_docs = FixedBitSet::with_set_prefix(max_doc, max_doc);

      for i in 0..self.num_deleted_doc_ids {
        live_docs.clear_with_index(self.delete_doc_ids[i] as usize)?;
      }

      flush_state.live_docs = Some(Arc::new(live_docs));
      flush_state.del_count_on_flush = self.num_deleted_doc_ids as i32;
      self.delete_doc_ids.clear();
      self.num_deleted_doc_ids = 0;
    }

    if self.state.aborted.load(Ordering::SeqCst) {
      if self.info_stream.is_enabled("DWPT") {
        self
          .info_stream
          .message("DWPT", "flush: skip because aborting is set")?;
      }
      return Ok(None);
    }

    let t0 = std::time::Instant::now();

    if self.info_stream.is_enabled("DWPT") {
      self.info_stream.message(
        "DWPT",
        &format!(
          "flush postings as segment {} numDocs={}",
          self.segment_info.name,
          self.state.num_docs_in_ram.load(SeqCst)
        ),
      )?;
    }

    let body_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
      || -> Result<Option<FlushedSegment<D>>> {
        // Rust Lucene requires all DocValuesWriter instances to be finished before
        // DocValuesWriter::get_doc_values can be called through an immutable reference.
        self.indexing_chain.finish_doc_values_writer()?;
        let mut soft_deleted_docs =
          if let Some(field) = index_writer_config.get_soft_deletes_field() {
            self.indexing_chain.get_has_doc_values(field)?
          } else {
            None
          };
        let sort_map = self.indexing_chain.flush(
          &mut flush_state,
          &mut self.segment_info,
          Some(&mut self.pending_updates),
          index_writer_config,
        )?;
        flush_state.soft_del_count_on_flush = if let Some(ref mut iter) = soft_deleted_docs {
          let cnt = count_soft_deletes(Some(iter), flush_state.live_docs.as_ref())?;
          debug_assert!(self.segment_info.max_doc()? >= (cnt + flush_state.del_count_on_flush));
          cnt
        } else {
          0
        };

        // We clear this here because we already resolved them (private to this segment) when writing
        // postings:
        self.pending_updates.clear_delete_terms();
        let files = self.directory.take_created_files();
        self.segment_info.replace_files(files)?;

        // Keep the original files available until the flush succeeds.
        let replacement_segment_info = self.segment_info.flush_failure_mirror()?;
        let segment_info_per_commit = SegmentCommitInfo::new(
          std::mem::replace(&mut self.segment_info, replacement_segment_info),
          0,
          flush_state.soft_del_count_on_flush,
          -1,
          -1,
          -1,
          Some(StringHelper::random_id()),
        );

        if self.info_stream.is_enabled("DWPT") {
          self.info_stream.message(
            "DWPT",
            &format!(
              "new segment has {} deleted docs",
              flush_state.del_count_on_flush
            ),
          )?;
          self.info_stream.message(
            "DWPT",
            &format!(
              "new segment has {} soft-deleted docs",
              flush_state.soft_del_count_on_flush
            ),
          )?;
          self.info_stream.message(
            "DWPT",
            &format!(
              "new segment has {}; {}; {}; {}; {}",
              if flush_state.field_infos.has_term_vectors() {
                "vectors"
              } else {
                "no vectors"
              },
              if flush_state.field_infos.has_norms() {
                "norms"
              } else {
                "no norms"
              },
              if flush_state.field_infos.has_doc_values() {
                "docValues"
              } else {
                "no docValues"
              },
              if flush_state.field_infos.has_prox() {
                "prox"
              } else {
                "no prox"
              },
              if flush_state.field_infos.has_freq() {
                "freqs"
              } else {
                "no freqs"
              }
            ),
          )?;
          self.info_stream.message(
            "DWPT",
            &format!("flushedFiles={:?}", segment_info_per_commit.files()),
          )?;
          self.info_stream.message(
            "DWPT",
            &format!("flushed codec={}", index_writer_config.get_codec()),
          )?;
        }

        let has_segment_updates = !self.pending_updates.delete_queries.is_empty()
          || self
            .pending_updates
            .num_field_updates
            .load(Ordering::SeqCst)
            != 0;
        if !has_segment_updates {
          self.pending_updates.clear();
        }

        if self.info_stream.is_enabled("DWPT") {
          let new_size_mb = segment_info_per_commit.size_in_bytes()? as f64 / 1024.0 / 1024.0;
          self.info_stream.message(
            "DWPT",
            &format!(
              "flushed: segment={} ramUsed={:.2} MB newFlushedSize={:.2} MB docs/MB={:.2}",
              segment_info_per_commit.info.name,
              start_mb_used,
              new_size_mb,
              segment_info_per_commit.info.max_doc()? as f64 / new_size_mb
            ),
          )?;
        }

        let mut fs = FlushedSegment::new(
          self.info_stream.clone(),
          segment_info_per_commit,
          flush_state.field_infos,
          has_segment_updates.then_some(&mut self.pending_updates),
          flush_state.live_docs.take(),
          flush_state.del_count_on_flush,
          sort_map,
        )?;
        if has_segment_updates {
          self.pending_updates.clear();
        }
        self.seal_flushed_segment(&mut fs, flush_notifications, index_writer_config)?;

        if self.info_stream.is_enabled("DWPT") {
          self.info_stream.message(
            "DWPT",
            &format!("flush time {} ms", t0.elapsed().as_millis()),
          )?;
        }

        Ok(Some(fs))
      },
    ));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
      || -> Result<Option<FlushedSegment<D>>> {
        if let Some(throwable) =
          body_result.clone_caught_failure("panic while handling an aborting flush error")
        {
          self.on_aborting_exception(throwable);
        }
        unwrap_caught_result!(body_result)
      },
    ));
    self.maybe_abort("flush", flush_notifications, writer)?;
    self
      .state
      .has_flushed
      .set(true)
      .map_err(|_| LuceneError::illegal_state("flush already called"))?;

    unwrap_caught_result!(result)
  }

  fn maybe_abort<FN>(
    &mut self,
    location: &str,
    flush_notifications: &FN,
    writer: &IndexWriter<D>,
  ) -> Result<()>
  where
    FN: FlushNotifications,
  {
    if self.aborting_exception.get().is_none() || self.state.aborted.load(Ordering::SeqCst) {
      return Ok(());
    }
    // if we are not already aborted, we can abort
    let event = self
      .aborting_exception
      .get()
      .ok_or_else(|| LuceneError::illegal_state("aborting error must be present"))?
      .clone_caught_failure("panic from an aborting error")
      .ok_or_else(|| LuceneError::illegal_state("aborting error must contain a failure"))?;
    let abort_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.abort()));
    flush_notifications.on_tragic_event(&event, location, writer)?;
    unwrap_caught_result!(abort_result)
  }
  pub(crate) fn take_pending_files_to_delete(&mut self) -> HashSet<String> {
    std::mem::take(&mut self.files_to_delete)
  }
  fn sort_live_docs<T, T2>(live_docs: &T, sort_map: &T2) -> Result<FixedBitSet>
  where
    T: Bits,
    T2: DocMap,
  {
    let live_docs_len = live_docs.length();
    let mut sorted_live_docs = FixedBitSet::with_set_prefix(live_docs_len, live_docs_len);

    for i in 0..live_docs_len {
      if !live_docs.get(i)? {
        let v: i32 = i.try_convert()?;
        sorted_live_docs.clear_with_index(sort_map.old_to_new(v)? as usize)?;
      }
    }
    Ok(sorted_live_docs)
  }
  /// Seals the [`SegmentInfo`](crate::core::index::segment_info::SegmentInfo) for the new flushed segment and persists the deleted documents [`FixedBitSet`].
  pub(crate) fn seal_flushed_segment<FN, T>(
    &mut self,
    flushed_segment: &mut FlushedSegment<D>,
    flush_notifications: &FN,
    index_writer_config: &T,
  ) -> Result<()>
  where
    FN: FlushNotifications,
    T: LiveIndexWriterConfig,
  {
    let new_segment = &mut flushed_segment.segment_info;
    let sort_map = flushed_segment.sort_map.as_deref();
    let res: Result<()> = (|| {
      let segment_info = Arc::get_mut(&mut new_segment.info).ok_or_else(|| {
        LuceneError::illegal_state("flushed segment info must be uniquely owned while sealing")
      })?;
      set_diagnostics(segment_info, SOURCE_FLUSH);
      let context = IOContext::with_flush(FlushInfo::new(
        new_segment.info.max_doc()?,
        new_segment.size_in_bytes()?,
      ))?;

      let mut success = false;
      let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        if index_writer_config.get_use_compound_file() {
          let segment_info = Arc::get_mut(&mut new_segment.info).ok_or_else(|| {
            LuceneError::illegal_state("flushed segment info must be uniquely owned while sealing")
          })?;
          let mut dir = TrackingDirectoryWrapper::new(self.directory.as_ref());
          let original_files = create_compound_file(
            &self.info_stream,
            &mut dir,
            segment_info,
            &context,
            IOConsumerImpl::new(flush_notifications),
          )?;
          self.files_to_delete.extend(original_files);
          segment_info.set_use_compound_file(true);
        }

        // Have codec write SegmentInfo.  Must do this after
        // creating CFS so that 1) .si isn't slurped into CFS,
        // and 2) .si reflects useCompoundFile=true change
        // above:
        let segment_info = Arc::get_mut(&mut new_segment.info).ok_or_else(|| {
          LuceneError::illegal_state("flushed segment info must be uniquely owned while sealing")
        })?;
        let codec = segment_info.get_codec()?;
        codec
          .segment_info_format()
          .write(self.directory.as_ref(), segment_info, &context)?;

        // Must write deleted docs after the CFS so we don't
        // slurp the del file into CFS:
        if let Some(live_docs) = &flushed_segment.live_docs {
          let del_count = flushed_segment.del_count;
          debug_assert!(del_count > 0);

          if self.info_stream.is_enabled("DWPT") {
            self.info_stream.message(
              "DWPT",
              &format!(
                "flush: write {} deletes gen={}",
                del_count,
                new_segment.get_del_gen()
              ),
            )?;
          }
          let codec = new_segment.info.get_codec()?;
          match sort_map {
            Some(map) => {
              codec.live_docs_format().write_live_docs(
                &Self::sort_live_docs(live_docs, map)?,
                self.directory.as_ref(),
                new_segment,
                del_count,
                &context,
              )?;
            },
            None => {
              codec.live_docs_format().write_live_docs(
                live_docs,
                self.directory.as_ref(),
                new_segment,
                del_count,
                &context,
              )?;
            },
          }

          new_segment.set_del_count(del_count)?;
          new_segment.advance_del_gen();
        }

        success = true;
        Ok(())
      }));
      // The replacement SegmentInfo is only needed for failed-flush cleanup;
      // successful flushes return before the mirror can be observed again.
      let mirror_result = if matches!(&result, Ok(Err(_)) | Err(_)) {
        new_segment
          .info
          .files()
          .and_then(|files| self.segment_info.set_files(files))
      } else {
        Ok(())
      };
      if !success && self.info_stream.is_enabled("DWPT") {
        self.info_stream.message(
          "DWPT",
          &format!(
            "encountered an error creating a compound file for newly flushed segment {}",
            new_segment.info.name
          ),
        )?;
      }
      match unwrap_caught_result!(result) {
        Ok(()) => mirror_result,
        Err(error) => Err(error),
      }
    })();
    res
  }

  #[allow(dead_code)]
  pub(crate) fn get_segment_info(&self) -> &SegmentInfo<D> {
    &self.segment_info
  }

  /// Returns true iff this DWPT is marked as flush pending
  pub(crate) fn is_flush_pending(&self) -> bool {
    self.state.is_flush_pending()
  }

  /// Sets this DWPT as flush pending. This can only be set once.
  #[allow(dead_code)]
  pub(crate) fn set_flush_pending(&self) -> Result<()> {
    self.state.set_flush_pending()
  }
  /// Returns the last committed bytes for this DWPT. This method can be called without acquiring the DWPT’s lock.
  pub(crate) fn get_last_committed_bytes_used(&self) -> i64 {
    self.state.last_committed_bytes_used.load(Ordering::SeqCst)
  }
  /// Commits the current [`ram_bytes_used()`](Self::ram_bytes_used) and stores its value for later reuse.
  /// The last committed bytes used can be retrieved via [`get_last_committed_bytes_used()`](Self::get_last_committed_bytes_used).
  pub(crate) fn commit_last_bytes_used(&self, delta: i64) -> Result<()> {
    debug_assert_eq!(
      self.get_commit_last_bytes_used_delta()?,
      delta,
      "delta has changed"
    );
    self
      .state
      .last_committed_bytes_used
      .fetch_add(delta, Ordering::SeqCst);
    Ok(())
  }
  /// Calculates the delta between the last committed bytes used and the currently used RAM.
  ///
  /// # Returns
  ///
  /// The difference between [`ram_bytes_used()`](Self::ram_bytes_used) and [`get_last_committed_bytes_used()`](Self::get_last_committed_bytes_used).
  ///
  /// # See
  ///
  /// [`commit_last_bytes_used()`](Self::commit_last_bytes_used)
  pub(crate) fn get_commit_last_bytes_used_delta(&self) -> Result<i64> {
    Ok(self.ram_bytes_used()? - self.state.last_committed_bytes_used.load(Ordering::SeqCst))
  }

  /// Returns `true` iff this DWPT has been flushed
  #[allow(dead_code)]
  pub(crate) fn has_flushed(&self) -> bool {
    self.state.has_flushed()
  }
}

impl<D> Accountable for DocumentsWriterPerThread<D>
where
  D: Directory,
{
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(
      size_of_vec(&self.delete_doc_ids)
        .saturating_add(self.pending_updates.ram_bytes_used()?)
        .saturating_add(self.indexing_chain.ram_bytes_used()?),
    )
  }

  fn get_child_resources(&self) -> Vec<&dyn Accountable> {
    vec![&self.pending_updates, &self.indexing_chain]
  }
}
impl<D> Display for DocumentsWriterPerThread<D>
where
  D: Directory,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
    write!(
      f,
      "{} [pendingDeletes={}, segment={}, aborted={}, numDocsInRAM={}, deleteQueue={}, {} deleted docIds]",
      std::any::type_name::<Self>(),
      self.pending_updates,
      self.segment_info.name,
      self.state.aborted.load(Ordering::SeqCst),
      self.state.num_docs_in_ram.load(SeqCst),
      self.state.delete_queue,
      self.num_deleted_doc_ids,
    )
  }
}

pub(crate) struct FlushedSegment<D> {
  pub(crate) segment_info: SegmentCommitInfo<D>,
  pub(crate) field_infos: Arc<FieldInfos>,
  pub(crate) segment_updates: Option<FrozenBufferedUpdates>,
  live_docs: Option<Arc<FixedBitSet>>,
  pub(crate) sort_map: Option<Arc<DocMapImpl>>,
  del_count: i32,
}
impl<D> FlushedSegment<D>
where
  D: Directory,
{
  fn new(
    info_stream: InfoStreamMT,
    segment_info: SegmentCommitInfo<D>,
    field_infos: Arc<FieldInfos>,
    segment_updates: Option<&mut BufferedUpdates>,
    live_docs: Option<Arc<FixedBitSet>>,
    del_count: i32,
    sort_map: Option<Arc<DocMapImpl>>,
  ) -> Result<Self> {
    let segment_updates = match segment_updates {
      Some(upd) if upd.any() => Some(FrozenBufferedUpdates::new(
        info_stream,
        upd,
        Some(Arc::clone(&segment_info.info.id_key)),
      )?),
      _ => None,
    };

    Ok(FlushedSegment {
      segment_info,
      field_infos,
      segment_updates,
      live_docs,
      del_count,
      sort_map,
    })
  }
}

pub struct IOConsumerImpl<'a, FN> {
  flush_notifications: &'a FN,
}
impl<'a, FN> IOConsumerImpl<'a, FN> {
  pub(crate) fn new(flush_notifications: &'a FN) -> Self {
    IOConsumerImpl {
      flush_notifications,
    }
  }
}
impl<FN> IOConsumer<HashSet<String>> for IOConsumerImpl<'_, FN>
where
  FN: FlushNotifications,
{
  fn accept(&mut self, input: HashSet<String>) -> Result<()> {
    self.flush_notifications.delete_unused_files(input)
  }
}
