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
use std::fmt::{Display, Formatter, Write as _};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

use parking_lot::Mutex;

use crate::core::index::buffered_updates::{BufferedUpdates, MAX_INT};
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::doc_values_update::{DocValuesUpdate, DocValuesUpdateBase};
use crate::core::index::frozen_buffered_updates::FrozenBufferedUpdates;
use crate::core::index::term::Term;
use crate::core::search::query::{Query, QueryBase};
use crate::core::util::accountable::Accountable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::info_stream::InfoStreamMT;

/// [`DocumentsWriterDeleteQueue`] is a non-blocking linked pending deletes
/// queue. Unlike other queue implementations, we only maintain the tail of the
/// queue. The delete queue is always used in a context of a set of
/// [`DocumentsWriterPerThread`](crate::core::index::documents_writer_per_thread::DocumentsWriterPerThread)
/// instances and a global delete pool. Each DWPT and the global pool need to
/// maintain their 'own' head of the queue (as a [`DeleteSlice`](DeleteSlice)
/// instance per
/// [`DocumentsWriterPerThread`](crate::core::index::documents_writer_per_thread::DocumentsWriterPerThread)).
/// The differences between DWPT and the global pool are:
///
/// - DWPT starts maintaining a head after adding its first document (since for
///   its segment-private deletes, only the deletes after that document are
///   relevant)
/// - The global pool starts maintaining the head immediately upon instance
///   creation by taking the sentinel instance as its initial head
///
/// Since each [`DeleteSlice`](DeleteSlice) maintains its own head and the list
/// is singly-linked, garbage collection prunes the list automatically. All
/// nodes in the list that remain relevant should be directly or indirectly
/// referenced by either:
///
/// - A DWPT's private [`DeleteSlice`](DeleteSlice)
/// - The global [`BufferedUpdates`](BufferedUpdates) slice
///
/// Each DWPT and the global delete pool maintain their private
/// [`DeleteSlice`](DeleteSlice) instance. For DWPT, updating a slice is
/// equivalent to atomically finalizing a document. The slice update guarantees
/// a "happens before" relationship to all other updates in the same indexing
/// session. When a DWPT updates a document:
///
/// 1. Consumes a document and finishes its processing
/// 2. Updates its private [`DeleteSlice`](DeleteSlice) through either:
///    - [`update_slice`](DocumentsWriterDeleteQueue::update_slice), or
///    - [`add_with_slice`](DocumentsWriterDeleteQueue::add_with_slice) (if the
///      document has a delTerm)
/// 3. Applies all deletes in the slice to its private
///    [`BufferedUpdates`](BufferedUpdates) and resets it
/// 4. Increments its internal document ID
///
/// The DWPT doesn't apply its current document's delete term until it updates
/// its delete slice, ensuring update consistency. If the update fails before
/// updating the [`DeleteSlice`], the delTerm won't be added to either its
/// private deletes or the global deletes.
///
/// # Type References
/// [`BufferedUpdates`](BufferedUpdates)
/// [`DeleteSlice`](crate::core::index::DeleteSlice)
/// [`DocumentsWriterPerThread`](crate::core::index::documents_writer_per_thread::DocumentsWriterPerThread)
pub struct DocumentsWriterDeleteQueue {
  // Serializes queue operations that share this monitor.
  queue_lock: Mutex<QueueState>,
  // The current end (latest delete operation) in the delete queue.
  tail: Mutex<Arc<Node>>,
  closed: AtomicBool,
  // Only acquired to update the global deletes, crate-visible for access by tests.
  pub(crate) global_buffer_lock: Mutex<GlobalBufferState>,
  pub(crate) generation: i64,
  /// Generates the sequence number that IW returns to callers changing the
  /// index, showing the effective serialization of all operations.
  next_seq_no: Arc<AtomicI64>,
  info_stream: InfoStreamMT,
  max_seq_no: AtomicI64,
  start_seq_no: i64,
  previous_max_seq_id: PreviousMaxSeqId,
}
enum PreviousMaxSeqId {
  Fixed(i64),
  FromNextSeqNo(Arc<AtomicI64>),
}

impl PreviousMaxSeqId {
  fn get(&self) -> i64 {
    match self {
      PreviousMaxSeqId::Fixed(value) => *value,
      PreviousMaxSeqId::FromNextSeqNo(next_seq_no) => next_seq_no.load(Ordering::SeqCst) - 1,
    }
  }
}

struct QueueState {
  advanced: bool,
}

impl QueueState {
  fn new() -> Self {
    Self { advanced: false }
  }
}

pub(crate) struct GlobalBufferState {
  /// Used to record deletes against all prior (already written to disk)
  /// segments. Whenever any segment flushes, we bundle up this set of
  /// deletes and insert into the buffered updates stream before the
  /// newly flushed segment(s).
  global_slice: DeleteSlice,
  global_buffered_updates: BufferedUpdates,
}

impl GlobalBufferState {
  fn new(tail: Arc<Node>) -> Self {
    Self {
      global_slice: DeleteSlice::new(tail),
      global_buffered_updates: BufferedUpdates::new(),
    }
  }

  pub(crate) fn apply(&mut self, doc_id_upto: i32) -> Result<()> {
    self
      .global_slice
      .apply(&mut self.global_buffered_updates, doc_id_upto)
  }
}
impl DocumentsWriterDeleteQueue {
  pub(crate) fn new(info_stream: InfoStreamMT) -> Self {
    Self::with_params(info_stream, 0, 1, 0)
  }
  pub(crate) fn with_params(
    info_stream: InfoStreamMT,
    generation: i64,
    start_seq_no: i64,
    previous_max_seq_id: i64,
  ) -> Self {
    Self::with_previous_max_seq_id(
      info_stream,
      generation,
      start_seq_no,
      PreviousMaxSeqId::Fixed(previous_max_seq_id),
    )
  }

  fn with_previous_max_seq_id(
    info_stream: InfoStreamMT,
    generation: i64,
    start_seq_no: i64,
    previous_max_seq_id: PreviousMaxSeqId,
  ) -> Self {
    let tail = Arc::new(Node::new(NodeEnum::EmptyNode(EmptyNode::new())));
    let value = previous_max_seq_id.get();
    debug_assert!(
      value <= start_seq_no,
      "illegal max sequence ID: {value} start was: {start_seq_no}"
    );

    Self {
      queue_lock: Mutex::new(QueueState::new()),
      tail: Mutex::new(tail.clone()),
      closed: AtomicBool::new(false),
      global_buffer_lock: Mutex::new(GlobalBufferState::new(tail)),
      generation,
      next_seq_no: Arc::new(AtomicI64::new(start_seq_no)),
      info_stream,
      max_seq_no: AtomicI64::new(i64::MAX),
      start_seq_no,
      previous_max_seq_id,
    }
  }
  pub(crate) fn add_delete_query(&self, queries: Vec<Query>) -> Result<i64> {
    let query_array_node = Node::new(NodeEnum::QueryNodeArray(QueryNodeArray::new(queries)));
    let seq_no = self.add(Arc::new(query_array_node))?;
    self.try_apply_global_slice()?;
    Ok(seq_no)
  }
  pub(crate) fn add_delete_term(&self, terms: Vec<Term>) -> Result<i64> {
    let node = Node::new(NodeEnum::TermNodeArray(TermNodeArray::new(terms)));
    let seq_no = self.add(Arc::new(node))?;
    self.try_apply_global_slice()?;
    Ok(seq_no)
  }
  pub(crate) fn add_doc_values_updates(&self, updates: Vec<DocValuesUpdate>) -> Result<i64> {
    let node = Node::new(NodeEnum::DocValuesUpdatesNode(DocValuesUpdatesNode::new(
      updates,
    )));
    let seq_no = self.add(Arc::new(node))?;
    self.try_apply_global_slice()?;
    Ok(seq_no)
  }
  pub(crate) fn new_node_with_term(term: Term) -> Node {
    Node::new(NodeEnum::TermNode(TermNode::new(term)))
  }

  pub(crate) fn new_node_with_query(query: Query) -> Node {
    Node::new(NodeEnum::QueryNode(Box::new(QueryNode::new(query))))
  }

  pub(crate) fn new_node_with_doc_values(updates: Vec<DocValuesUpdate>) -> Node {
    Node::new(NodeEnum::DocValuesUpdatesNode(DocValuesUpdatesNode::new(
      updates,
    )))
  }
  /// invariant for document update
  pub(crate) fn add_with_slice(
    &self,
    delete_node: Arc<Node>,
    slice: &mut DeleteSlice,
  ) -> Result<i64> {
    let seq_no = self.add(delete_node.clone())?;
    // This is an update request where the term is the updated documents
    // delTerm. In that case we need to guarantee that this insert is atomic
    // with regards to the given delete slice. This means if two threads try
    // to update the same document with in turn the same delTerm one
    // of them must win. By taking the node we have created for our
    // del term as the new tail it is guaranteed that if another
    // thread adds the same right after us we will apply this delete
    // next time we update our slice and one of the two
    // competing updates wins!
    slice.slice_tail = delete_node;
    debug_assert!(
      !Arc::ptr_eq(&slice.slice_head, &slice.slice_tail),
      "slice head and tail must differ after add"
    );
    self.try_apply_global_slice()?;
    Ok(seq_no)
  }

  pub(crate) fn add(&self, new_node: Arc<Node>) -> Result<i64> {
    let _queue_lock = self.queue_lock.lock();
    self.ensure_open()?;
    {
      let mut tail = self.tail.lock();
      let mut tail_next_guard = tail.next.lock();
      *tail_next_guard = Option::from(new_node.clone());
      drop(tail_next_guard);
      *tail = new_node;
    }

    Ok(self.get_next_sequence_number())
  }

  pub(crate) fn any_changes(&self, global_state: Option<&GlobalBufferState>) -> bool {
    let global_state = match global_state {
      Some(global_state) => global_state,
      None => &*self.global_buffer_lock.lock(),
    };
    let current_tail = self.tail.lock().clone();
    //  Check if all items in the global slice were applied,
    //  if the global slice is up-to-date,
    //  and if `global_buffered_updates` has changes.
    global_state.global_buffered_updates.any()
      || !global_state.global_slice.is_empty()
      || !Arc::ptr_eq(&global_state.global_slice.slice_tail, &current_tail)
      || current_tail.next.lock().is_some()
  }

  pub(crate) fn try_apply_global_slice(&self) -> Result<()> {
    match self.global_buffer_lock.try_lock() {
      Some(mut global_state) => {
        self.ensure_open()?;
        // The global buffer must be locked, but we don't need to update
        // them if there is an update going on right
        // now. It is sufficient to apply the
        // deletes that have been added after the current in-flight
        // global slices tail the next time we can get
        // the lock!
        let updated = {
          let current_tail = self.tail.lock();
          Self::update_slice_no_seq_no(&mut global_state, &current_tail)
        };
        if updated {
          global_state.apply(MAX_INT)?;
        }
      },
      _ => {
        return Ok(());
      },
    }
    Ok(())
  }

  pub(crate) fn freeze_global_buffer(
    &self,
    caller_slice: Option<&mut DeleteSlice>,
  ) -> Result<Option<FrozenBufferedUpdates>> {
    let mut global_state = self.global_buffer_lock.lock();
    self.ensure_open()?;
    // Here we freeze the global buffer so we need to lock it, apply all
    // deletes in the queue and reset the global slice to let the GC prune
    // the queue.
    // Take the current tail make this local any
    let current_tail = self.tail.lock().clone();
    // Changes after this call are applied later
    // and not relevant here
    if let Some(slice) = caller_slice {
      // Update the callers slices so we are on the same page
      slice.slice_tail = current_tail.clone();
    }
    let result = self.freeze_global_buffer_internal(&mut global_state, current_tail)?;
    Ok(result)
  }
  /// This may freeze the global buffer unless the delete queue has already
  /// been closed. If the queue has been closed, this method will return
  /// `None`.
  pub(crate) fn maybe_freeze_global_buffer(&self) -> Result<Option<FrozenBufferedUpdates>> {
    let mut global_state = self.global_buffer_lock.lock();

    if !self.closed.load(Ordering::SeqCst) {
      // Here we freeze the global buffer so we need to lock it,
      //  apply all deletes in the queue and reset the global slice
      // to let the GC prune the queue.
      let current_tail = self.tail.lock().clone(); // Take the current tail and make this local
      self.freeze_global_buffer_internal(&mut global_state, current_tail)
    } else {
      debug_assert!(
        !self.any_changes(Some(&global_state)),
        "We are closed but have changes"
      );
      Ok(None)
    }
  }

  fn freeze_global_buffer_internal(
    &self,
    global_state: &mut GlobalBufferState,
    current_tail: Arc<Node>,
  ) -> Result<Option<FrozenBufferedUpdates>> {
    debug_assert!(self.global_buffer_lock.is_locked());
    if !Arc::ptr_eq(&global_state.global_slice.slice_tail, &current_tail) {
      global_state.global_slice.slice_tail = current_tail;
      global_state.apply(MAX_INT)?;
    }

    if global_state.global_buffered_updates.any() {
      let packet = FrozenBufferedUpdates::new(
        self.info_stream.clone(),
        &mut global_state.global_buffered_updates,
        None,
      )?;

      global_state.global_buffered_updates.clear();
      Ok(Some(packet))
    } else {
      Ok(None)
    }
  }
  pub(crate) fn new_slice(&self) -> DeleteSlice {
    let tail = self.tail.lock().clone();
    DeleteSlice::new(tail)
  }
  /// Negative result means there were new deletes since we last applied.
  pub(crate) fn update_slice(&self, slice: &mut DeleteSlice) -> Result<i64> {
    let _queue_lock = self.queue_lock.lock();
    self.ensure_open()?;
    let mut seq_no = self.get_next_sequence_number();
    let current_tail = {
      let current_tail = self.tail.lock();
      if Arc::ptr_eq(&slice.slice_tail, &current_tail) {
        None
      } else {
        Some(current_tail.clone())
      }
    };
    if let Some(current_tail) = current_tail {
      // new deletes arrived since we last checked
      slice.slice_tail = current_tail;
      seq_no = -seq_no;
    }
    Ok(seq_no)
  }

  /// Just like updateSlice, but does not assign a sequence number.
  fn update_slice_no_seq_no(
    global_state: &mut GlobalBufferState,
    current_tail: &Arc<Node>,
  ) -> bool {
    if !Arc::ptr_eq(&global_state.global_slice.slice_tail, current_tail) {
      // New deletes arrived since the last check
      global_state.global_slice.slice_tail = Arc::clone(current_tail);
      true
    } else {
      false
    }
  }

  fn ensure_open(&self) -> Result<()> {
    if self.closed.load(Ordering::SeqCst) {
      return Err(LuceneError::already_closed("already closed."));
    }
    Ok(())
  }
  pub(crate) fn is_open(&self) -> bool {
    !self.closed.load(Ordering::SeqCst)
  }

  pub(crate) fn get_next_sequence_number(&self) -> i64 {
    let seq_no = self.next_seq_no.fetch_add(1, Ordering::SeqCst);
    debug_assert!(
      seq_no <= self.max_seq_no.load(Ordering::SeqCst),
      "seq_no={} vs max_seq_no={}",
      seq_no,
      self.max_seq_no.load(Ordering::SeqCst)
    );
    seq_no
  }
  pub(crate) fn close(&self) -> Result<()> {
    let _queue_lock = self.queue_lock.lock();
    let global_state = self.global_buffer_lock.lock();

    if self.any_changes(Some(&global_state)) {
      return Err(LuceneError::illegal_state(
        "Can't close queue unless all changes are applied",
      ));
    }
    self.closed.store(true, Ordering::SeqCst);

    let seq_no = self.next_seq_no.load(Ordering::SeqCst);
    debug_assert!(
      seq_no <= self.max_seq_no.load(Ordering::SeqCst),
      "maxSeqNo must be greater or equal to {} but was {}",
      seq_no,
      self.max_seq_no.load(Ordering::SeqCst)
    );
    self.next_seq_no.store(
      self.max_seq_no.load(Ordering::SeqCst).wrapping_add(1),
      Ordering::SeqCst,
    );
    Ok(())
  }
  #[cfg(test)]
  pub(crate) fn num_global_term_deletes(&self) -> i32 {
    let global_state = self.global_buffer_lock.lock();
    global_state.global_buffered_updates.delete_terms.size()
  }
  pub(crate) fn clear(&self) {
    let mut global_state = self.global_buffer_lock.lock();
    let current_tail = self.tail.lock().clone();
    global_state.global_slice.slice_head = current_tail.clone();
    global_state.global_slice.slice_tail = current_tail;
    global_state.global_buffered_updates.clear();
  }
  #[cfg(test)]
  pub(crate) fn get_buffered_updates_terms_size(&self) -> Result<i32> {
    let mut global_state = self.global_buffer_lock.lock();
    let current_tail = self.tail.lock().clone();

    if !Arc::ptr_eq(&global_state.global_slice.slice_tail, &current_tail) {
      global_state.global_slice.slice_tail = current_tail;
      global_state.apply(MAX_INT)?;
    }
    Ok(global_state.global_buffered_updates.delete_terms.size())
  }
  pub(crate) fn get_last_sequence_number(&self) -> i64 {
    self.next_seq_no.load(Ordering::SeqCst) - 1
  }
  /// Inserts a gap in the sequence numbers.
  /// This is used by IW during flush or commit to ensure any in-flight
  /// threads get sequence numbers inside the gap.
  pub(crate) fn skip_sequence_numbers(&self, jump: i64) {
    self.next_seq_no.fetch_add(jump, Ordering::SeqCst);
  }

  /// Returns the maximum completed sequence number for this queue.
  pub(crate) fn get_max_completed_seq_no(&self) -> i64 {
    let seq_no = self.next_seq_no.load(Ordering::SeqCst);

    if self.start_seq_no < seq_no {
      self.get_last_sequence_number()
    } else {
      // If we haven't advanced the seqNo, fall back to the previous queue
      let value = self.previous_max_seq_id.get();
      debug_assert!(
        value < self.start_seq_no,
        "illegal max sequence ID: {} start was: {}",
        value,
        self.start_seq_no
      );
      value
    }
  }

  /// Advances the queue to the next queue on flush. This carries over the
  /// generation to the next queue and sets the maximum sequence number
  /// based on the given `max_num_pending_ops`. This method can only be
  /// called once; subsequently, the returned queue should be used.
  ///
  /// # Arguments
  /// - `max_num_pending_ops`: The maximum number of possible concurrent
  ///   operations that will execute on this queue after it was advanced.
  ///
  /// # Returns
  /// A new `DocumentsWriterDeleteQueue` as the successor of this queue.
  pub(crate) fn advance_queue(
    &self,
    max_num_pending_ops: i64,
  ) -> Result<DocumentsWriterDeleteQueue> {
    let mut queue_state = self.queue_lock.lock();
    if queue_state.advanced {
      return Err(LuceneError::illegal_state("queue was already advanced"));
    }
    queue_state.advanced = true;

    let seq_no = self.get_last_sequence_number() + max_num_pending_ops + 1;

    self.max_seq_no.store(seq_no, Ordering::SeqCst);

    // Use an associated function to obtain this closure because we previously
    // introduced a memory leak since it would
    // implicitly reference this.nextSeqNo which holds on to this del queue.
    // see LUCENE-9478 for reference
    // Create a new queue with updated parameters
    Ok(DocumentsWriterDeleteQueue::with_previous_max_seq_id(
      self.info_stream.clone(),
      self.generation + 1,
      seq_no + 1,
      PreviousMaxSeqId::FromNextSeqNo(Arc::clone(&self.next_seq_no)),
    ))
  }

  /// Returns the maximum sequence number for this queue.
  /// This value will change once this queue is advanced.
  pub(crate) fn get_max_seq_no(&self) -> i64 {
    self.max_seq_no.load(Ordering::SeqCst)
  }

  /// Returns `true` if the queue has been advanced.
  pub(crate) fn is_advanced(&self) -> bool {
    self.queue_lock.lock().advanced
  }
}

impl Display for DocumentsWriterDeleteQueue {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "DWDQ: [ generation: {} ]", self.generation)
  }
}
impl Accountable for DocumentsWriterDeleteQueue {
  fn ram_bytes_used(&self) -> Result<i64> {
    let global_state = self.global_buffer_lock.lock();
    global_state.global_buffered_updates.ram_bytes_used()
  }
}

/// A delete slice for buffered updates.
pub(crate) struct DeleteSlice {
  slice_head: Arc<Node>, // Head of the slice
  slice_tail: Arc<Node>, // Tail of the slice
}

impl DeleteSlice {
  /// Creates a new delete slice with the head and tail pointing to the same
  /// node.
  pub(crate) fn new(current_tail: Arc<Node>) -> Self {
    Self {
      // Initially this is a 0 length slice pointing to the 'current' tail
      // of the queue. Once we update the slice we only need
      // to assign the tail and have a new slice
      slice_head: current_tail.clone(),
      slice_tail: current_tail,
    }
  }

  pub(crate) fn apply(&mut self, del: &mut BufferedUpdates, doc_id_upto: i32) -> Result<()> {
    if Arc::ptr_eq(&self.slice_head, &self.slice_tail) {
      // 0 length slice
      return Ok(());
    }
    // When we apply a slice we take the head and get its next as our first
    //item to apply and continue until we applied the tail. If the head and
    //tail in this slice are not equal then there will be at least one more
    //present node in the slice!
    {
      let mut current = &self.slice_head;
      let mut current_owner;
      loop {
        let next_node_guard = current.next.lock();
        debug_assert!(
          next_node_guard.is_some(),
          "slice property violated: nodes between the head and tail must not be null"
        );
        let next_node = next_node_guard.as_ref().ok_or_else(|| {
          LuceneError::illegal_state(
            "slice property violated: nodes between the head and tail must not be null",
          )
        })?;

        if Arc::ptr_eq(next_node, &self.slice_tail) {
          drop(next_node_guard);
          self.slice_tail.apply(del, doc_id_upto)?;
          break;
        }
        let next_node = Arc::clone(next_node);
        drop(next_node_guard);
        next_node.apply(del, doc_id_upto)?;
        current_owner = next_node;
        current = &current_owner;
      }
    }
    self.reset();
    Ok(())
  }
  pub(crate) fn reset(&mut self) {
    // Reset to a 0 length slice
    if !Arc::ptr_eq(&self.slice_head, &self.slice_tail) {
      self.slice_head = self.slice_tail.clone();
    }
  }
  /// Returns `true` if the given node is the slice's tail.
  pub(crate) fn is_tail(&self, node: &Arc<Node>) -> bool {
    Arc::ptr_eq(&self.slice_tail, node)
  }

  /// Returns `true` if the given item is identical to the item held by the
  /// tail.
  #[cfg(test)]
  pub(crate) fn is_tail_item(&self, item: &NodeEnum) -> bool {
    std::ptr::eq(&self.slice_tail.item, item)
  }

  pub(crate) fn is_empty(&self) -> bool {
    Arc::ptr_eq(&self.slice_head, &self.slice_tail)
  }
}

/// Represents a node in a linked list.
pub(crate) struct Node {
  /// The next node in the list, or `None` if this is the last node.
  next: Mutex<Option<Arc<Node>>>,
  pub(crate) item: NodeEnum,
}

impl Node {
  pub(crate) fn new(sub_node: NodeEnum) -> Self {
    Self {
      next: Mutex::new(None),
      item: sub_node,
    }
  }
}
impl Drop for Node {
  fn drop(&mut self) {
    let mut next = self.next.get_mut().take();
    while let Some(node) = next {
      match Arc::try_unwrap(node) {
        Ok(mut node) => next = node.next.get_mut().take(),
        Err(_) => break,
      }
    }
  }
}
impl NodeBase for Node {
  fn apply(&self, buffered_deletes: &mut BufferedUpdates, doc_id_upto: i32) -> Result<()> {
    self.item.apply(buffered_deletes, doc_id_upto)
  }

  fn is_delete(&self) -> bool {
    self.item.is_delete()
  }
}
impl Display for Node {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", self.item)
  }
}
// empty node
pub(crate) struct EmptyNode;
impl Default for EmptyNode {
  fn default() -> Self {
    Self::new()
  }
}

impl EmptyNode {
  pub(crate) fn new() -> Self {
    Self {}
  }
}
impl NodeBase for EmptyNode {
  fn apply(&self, _buffered_deletes: &mut BufferedUpdates, _doc_id_upto: i32) -> Result<()> {
    Err(LuceneError::illegal_state(
      "sentinel item must never be applied",
    ))
  }
}
impl Display for EmptyNode {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "")
  }
}
// term node
pub(crate) struct TermNode {
  item: Term,
}
impl TermNode {
  pub(crate) fn new(term: Term) -> Self {
    Self { item: term }
  }
}
impl NodeBase for TermNode {
  fn apply(&self, buffered_deletes: &mut BufferedUpdates, doc_id_upto: i32) -> Result<()> {
    buffered_deletes.add_term(&self.item, doc_id_upto)
  }
}
impl Display for TermNode {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "del={}", self.item)
  }
}
// query node
pub(crate) struct QueryNode {
  item: Query,
}
impl QueryNode {
  pub(crate) fn new(query: Query) -> Self {
    Self { item: query }
  }
}
impl NodeBase for QueryNode {
  fn apply(&self, buffered_deletes: &mut BufferedUpdates, doc_id_upto: i32) -> Result<()> {
    buffered_deletes.add_query(&self.item, doc_id_upto)
  }
}
impl Display for QueryNode {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    match self.item.to_string("") {
      Ok(s) => write!(f, "del={}", s),
      Err(_) => Err(std::fmt::Error),
    }
  }
}
// query node array
pub(crate) struct QueryNodeArray {
  item: Vec<Query>,
}
impl QueryNodeArray {
  pub(crate) fn new(nodes: Vec<Query>) -> Self {
    Self { item: nodes }
  }
}
impl NodeBase for QueryNodeArray {
  fn apply(&self, buffered_deletes: &mut BufferedUpdates, doc_id_upto: i32) -> Result<()> {
    for query in &self.item {
      buffered_deletes.add_query(query, doc_id_upto)?;
    }
    Ok(())
  }
}
impl Display for QueryNodeArray {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "")
  }
}

// term node array
pub(crate) struct TermNodeArray {
  item: Vec<Term>,
}

impl TermNodeArray {
  pub(crate) fn new(nodes: Vec<Term>) -> Self {
    Self { item: nodes }
  }
}
impl NodeBase for TermNodeArray {
  fn apply(&self, buffered_deletes: &mut BufferedUpdates, doc_id_upto: i32) -> Result<()> {
    for term in &self.item {
      buffered_deletes.add_term(term, doc_id_upto)?;
    }
    Ok(())
  }
}
impl Display for TermNodeArray {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "dels=[")?;
    for (i, term) in self.item.iter().enumerate() {
      if i > 0 {
        write!(f, ", ")?;
      }
      write!(f, "{term}")?;
    }
    write!(f, "]")
  }
}

// doc values update node

pub(crate) struct DocValuesUpdatesNode {
  item: Vec<DocValuesUpdate>,
}
impl DocValuesUpdatesNode {
  pub(crate) fn new(nodes: Vec<DocValuesUpdate>) -> Self {
    Self { item: nodes }
  }
}
impl NodeBase for DocValuesUpdatesNode {
  fn apply(&self, buffered_deletes: &mut BufferedUpdates, doc_id_upto: i32) -> Result<()> {
    for doc_values_update in &self.item {
      match doc_values_update.doc_values_type {
        DocValuesType::Binary => {
          buffered_deletes.add_binary_update(doc_values_update, doc_id_upto)?;
        },
        DocValuesType::Numeric => {
          buffered_deletes.add_numeric_update(doc_values_update, doc_id_upto)?;
        },
        _ => {
          Err(LuceneError::illegal_argument(format!(
            "{:?} DocValues updates not supported yet!",
            doc_values_update.doc_values_type
          )))?;
        },
      }
    }
    Ok(())
  }

  fn is_delete(&self) -> bool {
    false
  }
}
impl Display for DocValuesUpdatesNode {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    let mut sb = String::new();
    sb.push_str("docValuesUpdates: ");
    if !self.item.is_empty() {
      match &self.item[0].term {
        Some(term) => write!(sb, "term={term}; updates: [")?,
        None => sb.push_str("term=null; updates: ["),
      }
      for update in &self.item {
        write!(
          sb,
          "{}:{},",
          update.field,
          update.sub_update.value_to_string()
        )?;
      }
      if let Some(last_char) = sb.pop()
        && last_char != ','
      {
        sb.push(last_char);
      }
      sb.push(']');
    }
    write!(f, "{sb}")
  }
}

pub(crate) enum NodeEnum {
  EmptyNode(EmptyNode),
  TermNode(TermNode),
  QueryNode(Box<QueryNode>),
  QueryNodeArray(QueryNodeArray),
  TermNodeArray(TermNodeArray),
  DocValuesUpdatesNode(DocValuesUpdatesNode),
}

impl NodeBase for NodeEnum {
  fn apply(&self, buffered_deletes: &mut BufferedUpdates, doc_id_upto: i32) -> Result<()> {
    match self {
      NodeEnum::TermNode(node) => node.apply(buffered_deletes, doc_id_upto),
      NodeEnum::QueryNode(node) => node.apply(buffered_deletes, doc_id_upto),
      NodeEnum::QueryNodeArray(node) => node.apply(buffered_deletes, doc_id_upto),
      NodeEnum::TermNodeArray(node) => node.apply(buffered_deletes, doc_id_upto),
      NodeEnum::DocValuesUpdatesNode(node) => node.apply(buffered_deletes, doc_id_upto),
      NodeEnum::EmptyNode(node) => node.apply(buffered_deletes, doc_id_upto),
    }
  }
  fn is_delete(&self) -> bool {
    match self {
      NodeEnum::EmptyNode(node) => node.is_delete(),
      NodeEnum::TermNode(node) => node.is_delete(),
      NodeEnum::QueryNode(node) => node.is_delete(),
      NodeEnum::QueryNodeArray(node) => node.is_delete(),
      NodeEnum::TermNodeArray(node) => node.is_delete(),
      NodeEnum::DocValuesUpdatesNode(node) => node.is_delete(),
    }
  }
}
impl Display for NodeEnum {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    match self {
      NodeEnum::EmptyNode(node) => write!(f, "{node}"),
      NodeEnum::TermNode(node) => write!(f, "{node}"),
      NodeEnum::QueryNode(node) => write!(f, "{node}"),
      NodeEnum::QueryNodeArray(node) => write!(f, "{node}"),
      NodeEnum::TermNodeArray(node) => write!(f, "{node}"),
      NodeEnum::DocValuesUpdatesNode(node) => write!(f, "{node}"),
    }
  }
}

pub(crate) trait NodeBase {
  fn apply(&self, _buffered_deletes: &mut BufferedUpdates, _doc_id_upto: i32) -> Result<()> {
    Err(LuceneError::illegal_state(
      "sentinel item must never be applied",
    ))
  }

  #[allow(dead_code)]
  fn is_delete(&self) -> bool {
    true
  }
}
