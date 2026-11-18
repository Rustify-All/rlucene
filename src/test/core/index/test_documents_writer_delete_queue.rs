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
use crate::test_framework::core::index::test_concurrent_merge_scheduler::CountDownLatch;
use crate::test_framework::core::util::lucene_test_case::{random, random_multiplier};
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::{thread, vec};

use parking_lot::Mutex;
use rand::RngExt;

use crate::core::index::buffered_updates::{BufferedUpdates, BufferedUpdatesLock, MAX_INT};
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::doc_values_update::{
  BinaryDocValuesUpdate, DocValuesUpdate, DocValuesUpdateEnum,
};
use crate::core::index::documents_writer_delete_queue::{
  DeleteSlice, DocumentsWriterDeleteQueue, Node, NodeEnum, TermNodeArray,
};
use crate::core::index::field_term_iterator::FieldTermIterator;

use crate::core::index::term::Term;
use crate::core::index::{BytesRef, BytesRefBuilder};

use crate::core::search::term_query::TermQuery;

use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::error::lucene_error::{LuceneError, LuceneErrorKind, Result};
use crate::core::util::info_stream::get_default_info_stream;

#[allow(dead_code)] // for quick search
struct TestDocumentsWriterDeleteQueue;

#[test]
fn test_update_delete_slices() -> Result<()> {
  let mut random = random();
  let queue = DocumentsWriterDeleteQueue::new(get_default_info_stream());
  let size = (200 + random.random_range(0..500) * random_multiplier()) as usize;
  let mut ids: Vec<i32> = Vec::with_capacity(size);
  for _ in 0..size {
    ids.push(random.random());
  }
  let mut slice1 = queue.new_slice();
  let mut slice2 = queue.new_slice();
  let mut bd1 = BufferedUpdates::new();
  let mut bd2 = BufferedUpdates::new();
  let mut last1 = 0;
  let mut last2 = 0;
  let mut unique_values = HashSet::new();
  for (j, &id) in ids.iter().enumerate() {
    let term = Term::from_text("id", id.to_string());
    unique_values.insert(term.clone());
    // Retain the owning node so the moved array item has a stable identity.
    let node = Arc::new(Node::new(NodeEnum::TermNodeArray(TermNodeArray::new(
      vec![term],
    ))));
    queue.add(node.clone())?;
    queue.try_apply_global_slice()?;
    if random.random_range(0..20) == 0 || j == size - 1 {
      queue.update_slice(&mut slice1)?;
      assert!(slice1.is_tail_item(&node.item));
      slice1.apply(&mut bd1, j as i32)?;
      test_assert_all_between(last1 as i32, j as i32, &mut bd1, &ids)?;
      last1 = j + 1;
    }
    if random.random_range(0..10) == 5 || j == size - 1 {
      queue.update_slice(&mut slice2)?;
      assert!(slice2.is_tail_item(&node.item));
      slice2.apply(&mut bd2, j as i32)?;
      test_assert_all_between(last2 as i32, j as i32, &mut bd2, &ids)?;
      last2 = j + 1;
    }
    let num_deletes = queue.num_global_term_deletes() as usize;
    assert_eq!(unique_values.len(), num_deletes);
  }

  let bd1_terms_set: HashSet<Term> = bd1.delete_terms.key_set()?;
  let bd2_terms_set: HashSet<Term> = bd2.delete_terms.key_set()?;
  assert_eq!(unique_values, bd1_terms_set);
  assert_eq!(unique_values, bd2_terms_set);

  let frozen = queue.freeze_global_buffer(None)?.unwrap();
  let mut iter = frozen.delete_terms.iterator()?;
  let mut frozen_set: HashSet<Term> = HashSet::new();
  let mut bytes_ref = BytesRefBuilder::new();
  while let Some(byte_ref) = iter.next()? {
    bytes_ref.copy_bytes_from_ref(&byte_ref)?;
    let term = Term::new(iter.field().to_string(), bytes_ref.get_bytes_ref_copy()?);
    frozen_set.insert(term);
  }
  assert_eq!(unique_values, frozen_set);
  let num_deletes_after = queue.num_global_term_deletes();
  assert_eq!(0, num_deletes_after, "num deletes must be 0 after freeze");

  Ok(())
}

fn test_assert_all_between(
  start: i32,
  end: i32,
  deletes: &mut BufferedUpdates,
  ids: &[i32],
) -> Result<()> {
  for i in start..=end {
    let term = Term::from_text("id", ids[i as usize].to_string());
    assert_eq!(end, deletes.delete_terms.get(&term)?);
  }
  Ok(())
}
#[test]
fn test_clear() -> Result<()> {
  let mut random = random();
  let queue = DocumentsWriterDeleteQueue::new(get_default_info_stream());
  assert!(!queue.any_changes(None));
  queue.clear();
  assert!(!queue.any_changes(None));
  let size = 200 + random.random_range(0..500) * random_multiplier();
  for i in 0..size {
    let term = Term::from_text("id", i.to_string());
    if random.random_range(0..10) == 0 {
      queue.add_delete_query(Vec::from([TermQuery::new(term).into()]))?;
    } else {
      queue.add_delete_term(vec![term])?;
    }
    assert!(queue.any_changes(None));

    if random.random_range(0..10) == 0 {
      queue.clear();
      queue.try_apply_global_slice()?;
      assert!(!queue.any_changes(None));
    }
  }

  Ok(())
}
#[test]
fn test_any_changes() -> Result<()> {
  let mut random = random();
  let queue = DocumentsWriterDeleteQueue::new(get_default_info_stream());
  let size = 200 + random.random_range(0..500) * random_multiplier();
  let mut terms_since_freeze = 0;
  let mut queries_since_freeze = 0;

  for i in 0..size {
    let term = Term::from_text("id", i.to_string());
    if random.random_range(0..10) == 0 {
      queue.add_delete_query(vec![TermQuery::new(term).into()])?;
      queries_since_freeze += 1;
    } else {
      queue.add_delete_term(vec![term])?;
      terms_since_freeze += 1;
    }

    assert!(queue.any_changes(None));

    if random.random_range(0..5) == 0 {
      let frozen = queue.freeze_global_buffer(None)?.ok_or_else(|| {
        LuceneError::illegal_state("freeze_global_buffer returned None despite queued changes")
      })?;
      assert_eq!(terms_since_freeze, frozen.delete_terms.size());
      assert_eq!(queries_since_freeze, frozen.delete_queries.len());
      terms_since_freeze = 0;
      queries_since_freeze = 0;
      assert!(!queue.any_changes(None));
    }
  }
  Ok(())
}
#[test]
fn test_partially_applied_global_slice() -> Result<()> {
  let queue = Arc::new(DocumentsWriterDeleteQueue::new(get_default_info_stream()));
  let lock = queue.global_buffer_lock.lock();
  let handle = thread::spawn({
    let queue = queue.clone();
    move || {
      let term = Term::from_text("foo", "bar");
      queue.add_delete_term(vec![term]).unwrap();
    }
  });
  handle.join().unwrap();
  drop(lock);
  assert!(queue.any_changes(None));
  queue.try_apply_global_slice()?;
  assert!(queue.any_changes(None));
  let frozen_global_buffer_wrap = queue.freeze_global_buffer(None)?;
  assert!(frozen_global_buffer_wrap.is_some());
  let frozen_global_buffer = frozen_global_buffer_wrap.unwrap();
  assert!(frozen_global_buffer.any());
  assert_eq!(1, frozen_global_buffer.delete_terms.size());
  assert!(!queue.any_changes(None));
  Ok(())
}
#[test]
fn test_stress_delete_queue() -> Result<()> {
  let mut random = random();
  let queue = Arc::new(DocumentsWriterDeleteQueue::new(get_default_info_stream()));
  let mut unique_values = HashSet::new();
  let size = 10000 + random.random_range(0..500) * random_multiplier();
  let ids: Arc<[i32]> = (0..size)
    .map(|_| random.random())
    .collect::<Vec<_>>()
    .into();
  for id in ids.iter() {
    unique_values.insert(Term::from_text("id", id.to_string()));
  }

  let latch = CountDownLatch::new(1);
  let index = Arc::new(AtomicUsize::new(0));
  let num_threads = 2 + random.random_range(0..5);

  let mut threads = Vec::new();
  for _ in 0..num_threads {
    let thread = UpdateThread::new(
      Arc::clone(&queue),
      Arc::clone(&index),
      ids.clone(),
      latch.clone(),
    )?;
    threads.push(Arc::new(Mutex::new(thread)));
  }

  let mut handles = Vec::new();
  for thread in &threads {
    let thread = Arc::clone(thread);
    handles.push(thread::spawn(move || {
      let mut thread = thread.lock();
      thread.run().expect("Thread execution failed");
    }));
  }
  latch.count_down();
  for handle in handles {
    handle.join().expect("Thread join failed");
  }
  for thread in threads {
    let mut guard = thread.lock();
    queue.update_slice(&mut guard.slice)?;
    let deletes = guard.deletes.clone();
    let mut deletes_guard = deletes.lock();
    guard.slice.apply(&mut deletes_guard, MAX_INT)?;
    assert_eq!(unique_values, deletes_guard.delete_terms.key_set()?);
  }

  queue.try_apply_global_slice()?;
  let mut frozen_set = HashSet::new();
  let frozen = queue.freeze_global_buffer(None)?.unwrap();
  let mut iter = frozen.delete_terms.iterator()?;
  let mut builder = BytesRefBuilder::new();
  while let Some(byte_ref) = iter.next()? {
    builder.copy_bytes_from_ref(&byte_ref)?;
    let term = Term::new(iter.field().to_string(), builder.get_bytes_ref_copy()?);
    frozen_set.insert(term);
  }
  assert_eq!(unique_values.len(), frozen_set.len());
  assert_eq!(unique_values, frozen_set);
  assert_eq!(0, queue.num_global_term_deletes());
  Ok(())
}

#[test]
fn test_close() -> Result<()> {
  {
    let mut random = random();
    let queue = DocumentsWriterDeleteQueue::new(get_default_info_stream());
    assert!(queue.is_open());
    queue.close()?;
    if random.random_bool(0.5) {
      queue.close()?; // double close
    }
    let result = queue.add_delete_term(vec![Term::from_text("foo", "bar")]);
    assert!(matches!(
      result.as_ref().map_err(LuceneError::kind),
      Err(LuceneErrorKind::AlreadyClosed(_))
    ));
    let result = queue.freeze_global_buffer(None);
    assert!(matches!(
      result.as_ref().map_err(LuceneError::kind),
      Err(LuceneErrorKind::AlreadyClosed(_))
    ));
    let result = queue.add_delete_query(vec![TermQuery::new(Term::from_text("foo", "bar")).into()]);
    assert!(matches!(
      result.as_ref().map_err(LuceneError::kind),
      Err(LuceneErrorKind::AlreadyClosed(_))
    ));

    let sub_update = DocValuesUpdateEnum::Binary(BinaryDocValuesUpdate::new(Option::from(
      BytesRef::from_string(""),
    )));
    let update = DocValuesUpdate::new(
      DocValuesType::Binary,
      Term::from_text("id", "0"),
      "enabled",
      MAX_INT,
      sub_update,
    );
    let result = queue.add_doc_values_updates(vec![update]);
    assert!(matches!(
      result.as_ref().map_err(LuceneError::kind),
      Err(LuceneErrorKind::AlreadyClosed(_))
    ));
    let result = queue.maybe_freeze_global_buffer()?;
    assert!(result.is_none());
    assert!(!queue.is_open());
  }
  {
    let queue = DocumentsWriterDeleteQueue::new(get_default_info_stream());
    queue.add_delete_term(vec![Term::from_text("foo", "bar")])?;
    let result = queue.close();
    assert!(matches!(
      result,
      Err(error) if error.is_illegal_state_error()
    ));

    assert!(queue.is_open());
    queue.try_apply_global_slice()?;
    queue.freeze_global_buffer(None)?;
    queue.close()?;
    assert!(!queue.is_open());
  }
  Ok(())
}

struct UpdateThread {
  queue: Arc<DocumentsWriterDeleteQueue>,
  index: Arc<AtomicUsize>,
  ids: Arc<[i32]>,
  slice: DeleteSlice,
  deletes: BufferedUpdatesLock,
  latch: CountDownLatch,
}

impl UpdateThread {
  fn new(
    queue: Arc<DocumentsWriterDeleteQueue>,
    index: Arc<AtomicUsize>,
    ids: Arc<[i32]>,
    latch: CountDownLatch,
  ) -> Result<Self> {
    let slice = queue.new_slice();
    let deletes = Arc::new(Mutex::new(BufferedUpdates::new()));

    Ok(UpdateThread {
      queue,
      index,
      ids,
      slice,
      deletes,
      latch,
    })
  }
  fn run(&mut self) -> Result<()> {
    self.latch.wait();
    loop {
      let i = self.index.fetch_add(1, Ordering::SeqCst);
      if i >= self.ids.len() {
        break;
      }
      let term = Term::from_text("id", self.ids[i].to_string());
      let term_node = Arc::new(DocumentsWriterDeleteQueue::new_node_with_term(term));
      self
        .queue
        .add_with_slice(term_node.clone(), &mut self.slice)?;
      assert!(self.slice.is_tail(Arc::as_ptr(&term_node)));

      let mut guard = self.deletes.lock();
      self.slice.apply(&mut guard, MAX_INT)?;
    }
    Ok(())
  }
}
