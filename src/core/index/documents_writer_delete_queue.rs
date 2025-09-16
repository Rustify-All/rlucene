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
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use parking_lot::Mutex;

use crate::core::index::buffered_updates::{BufferedUpdates, MAX_INT, MTBufferedUpdates};
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::doc_values_update::{DocValuesUpdate, DocValuesUpdateBase};
use crate::core::index::frozen_buffered_updates::FrozenBufferedUpdates;
use crate::core::index::term::Term;
use crate::core::search::query::QueryEnum;
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
pub(crate) struct DocumentsWriterDeleteQueue {
    pub(crate) inner: Mutex<GlobalState>,
    pub(crate) generation: i64,
    /// Generates the sequence number that IW returns to callers changing the
    /// index, showing the effective serialization of all operations.
    next_seq_no: Arc<AtomicI64>,
    info_stream: InfoStreamMT,
    start_seq_no: i64,
    previous_max_seq_id: i64,
    max_seq_no: AtomicI64,
}
pub(crate) struct GlobalState {
    tail: Arc<Node>,
    /// Used to record deletes against all prior (already written to disk)
    /// segments. Whenever any segment flushes, we bundle up this set of
    /// deletes and insert into the buffered updates stream before the
    /// newly flushed segment(s).
    global_slice: DeleteSlice,

    generation: i64,
    global_buffered_updates: MTBufferedUpdates,
    advanced: bool,
    closed: bool,
}
impl GlobalState {
    fn new(tail: Arc<Node>, generation: i64) -> Self {
        Self {
            tail: tail.clone(),
            global_slice: DeleteSlice::new(tail),
            generation,
            global_buffered_updates: BufferedUpdates::new_sync("global"),
            advanced: false,
            closed: false,
        }
    }
}
impl GlobalState {
    pub(crate) fn apply(&mut self, doc_id_upto: i32) -> Result<()> {
        self.global_slice
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
        let tail = Arc::new(Node::new(NodeEnum::EmptyNode(EmptyNode::new())));
        let value = previous_max_seq_id;
        debug_assert!(
            value <= start_seq_no,
            "illegal max sequence ID: {value} start was: {start_seq_no}"
        );
        let global_slice = GlobalState::new(tail, generation);

        Self {
            inner: Mutex::new(global_slice),
            generation,
            next_seq_no: Arc::new(AtomicI64::new(start_seq_no)),
            info_stream,
            start_seq_no,
            previous_max_seq_id,
            max_seq_no: AtomicI64::new(i64::MAX),
        }
    }
    pub(crate) fn add_delete_query(&self, queries: Vec<Arc<QueryEnum>>) -> Result<i64> {
        let query_array_node = Node::new(NodeEnum::QueryNodeArray(QueryNodeArray::new(queries)));
        let seq_no = self.add_node(Arc::new(query_array_node))?;
        self.try_apply_global_slice()?;
        Ok(seq_no)
    }
    pub(crate) fn add_delete_term(&self, terms: Vec<Term>) -> Result<i64> {
        let node = Node::new(NodeEnum::TermNodeArray(TermNodeArray::new(terms)));
        let seq_no = self.add_node(Arc::new(node))?;
        self.try_apply_global_slice()?;
        Ok(seq_no)
    }
    pub(crate) fn add_doc_values_updates(&self, updates: Vec<DocValuesUpdate>) -> Result<i64> {
        let node = Node::new(NodeEnum::DocValuesUpdatesNode(DocValuesUpdatesNode::new(
            updates,
        )));
        let seq_no = self.add_node(Arc::new(node))?;
        self.try_apply_global_slice()?;
        Ok(seq_no)
    }
    pub(crate) fn new_node_with_term(term: Term) -> Node {
        Node::new(NodeEnum::TermNode(TermNode::new(term)))
    }

    pub(crate) fn new_node_with_query(query: QueryEnum) -> Node {
        Node::new(NodeEnum::QueryNode(QueryNode::new(Arc::new(query))))
    }

    pub(crate) fn new_node_with_doc_values(updates: &[DocValuesUpdate]) -> Node {
        Node::new(NodeEnum::DocValuesUpdatesNode(DocValuesUpdatesNode::new(
            updates.to_vec(),
        )))
    }
    /// invariant for document update
    pub(crate) fn add_with_slice(
        &self,
        delete_node: Arc<Node>,
        slice: &mut DeleteSlice,
    ) -> Result<i64> {
        let seq_no = self.add_node(delete_node.clone())?;
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
        // TODO doing this each time is not necessary maybe
        // we can do it just every n times or so?
        self.try_apply_global_slice()?;
        Ok(seq_no)
    }

    pub(crate) fn add_node(&self, new_node: Arc<Node>) -> Result<i64> {
        let mut global_state = self.inner.lock();
        self.ensure_open(global_state.closed)?;
        {
            let mut tail_next_guard = global_state.tail.next.lock();
            *tail_next_guard = Option::from(new_node.clone());
        }
        global_state.tail = new_node;

        Ok(self.get_next_sequence_number())
    }

    pub(crate) fn any_changes(&self, global_state: Option<&GlobalState>) -> bool {
        let global_state = match global_state {
            Some(state) => state,
            None => &self.inner.lock(),
        };
        //  Check if all items in the global slice were applied,
        //  if the global slice is up-to-date,
        //  and if `global_buffered_updates` has changes.
        global_state.global_buffered_updates.any()
            || !global_state.global_slice.is_empty()
            || !Arc::ptr_eq(&global_state.global_slice.slice_tail, &global_state.tail)
            || global_state.tail.next.lock().is_some()
    }

    pub(crate) fn try_apply_global_slice(&self) -> Result<()> {
        match self.inner.try_lock() {
            Some(mut global_state) => {
                self.ensure_open(global_state.closed)?;
                // The global buffer must be locked, but we don't need to update
                // them if there is an update going on right
                // now. It is sufficient to apply the
                // deletes that have been added after the current in-flight
                // global slices tail the next time we can get
                // the lock!
                if self.update_slice_no_seq_no(&mut global_state) {
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
        caller_slice: &mut Option<DeleteSlice>,
    ) -> Result<Option<FrozenBufferedUpdates>> {
        let mut global_state = self.inner.lock();
        self.ensure_open(global_state.closed)?;
        // Here we freeze the global buffer so we need to lock it, apply all
        // deletes in the queue and reset the global slice to let the GC prune
        // the queue.
        // Take the current tail make this local any
        let current_tail = global_state.tail.clone();
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
        let mut global_state = self.inner.lock();

        if !global_state.closed {
            // Here we freeze the global buffer so we need to lock it,
            //  apply all deletes in the queue and reset the global slice
            // to let the GC prune the queue.
            let current_tail = global_state.tail.clone(); // Take the current tail and make this local
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
        global_state: &mut GlobalState,
        current_tail: Arc<Node>,
    ) -> Result<Option<FrozenBufferedUpdates>> {
        debug_assert!(self.inner.is_locked());
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
        let global_state = self.inner.lock().tail.clone();
        DeleteSlice::new(global_state)
    }
    /// Negative result means there were new deletes since we last applied.
    pub(crate) fn update_slice(&self, slice: &mut DeleteSlice) -> Result<i64> {
        let global_state = self.inner.lock();
        self.ensure_open(global_state.closed)?;
        let mut seq_no = self.get_next_sequence_number();
        if !Arc::ptr_eq(&slice.slice_tail, &global_state.tail) {
            // new deletes arrived since we last checked
            slice.slice_tail = global_state.tail.clone();
            seq_no = -seq_no;
        }
        Ok(seq_no)
    }

    /// Just like updateSlice, but does not assign a sequence number.
    pub(crate) fn update_slice_no_seq_no(&self, global_state: &mut GlobalState) -> bool {
        if !Arc::ptr_eq(&global_state.global_slice.slice_tail, &global_state.tail) {
            // New deletes arrived since the last check
            global_state.global_slice.slice_tail = global_state.tail.clone();
            true
        } else {
            false
        }
    }

    fn ensure_open(&self, closed: bool) -> Result<()> {
        if closed {
            return Err(LuceneError::already_closed("already closed."));
        }
        Ok(())
    }
    pub(crate) fn is_open(&self) -> bool {
        let global_state = self.inner.lock();
        !global_state.closed
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
        let mut global_state = self.inner.lock();

        if self.any_changes(Some(&global_state)) {
            return Err(LuceneError::illegal_state(
                "Can't close queue unless all changes are applied",
            ));
        }
        global_state.closed = true;

        let seq_no = self.next_seq_no.load(Ordering::SeqCst);
        debug_assert!(
            seq_no <= self.max_seq_no.load(Ordering::SeqCst),
            "maxSeqNo must be greater or equal to {} but was {}",
            seq_no,
            self.max_seq_no.load(Ordering::SeqCst)
        );
        self.next_seq_no
            .store(self.max_seq_no.load(Ordering::SeqCst) + 1, Ordering::SeqCst);
        Ok(())
    }
    #[cfg(debug_assertions)]
    pub(crate) fn num_global_term_deletes(&self) -> i32 {
        let global_state = self.inner.lock();
        global_state.global_buffered_updates.delete_terms.size()
    }
    pub(crate) fn clear(&self) {
        let mut global_state = self.inner.lock();
        global_state.global_slice.slice_head = global_state.tail.clone();
        global_state.global_slice.slice_tail = global_state.tail.clone();
        global_state.global_buffered_updates.clear();
    }
    pub(crate) fn get_buffered_updates_terms_size(&self) -> Result<i32> {
        let mut global_state = self.inner.lock();

        let current_tail = global_state.tail.clone();

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
            let value = self.previous_max_seq_id;
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
        let mut global_state = self.inner.lock();
        if global_state.advanced {
            return Err(LuceneError::illegal_state("queue was already advanced"));
        }
        global_state.advanced = true;

        let seq_no = self.get_last_sequence_number() + max_num_pending_ops + 1;

        self.max_seq_no.store(seq_no, Ordering::SeqCst);

        // we use a static method to get this lambda since we previously
        // introduced a memory leak since it would
        // implicitly reference this.nextSeqNo which holds on to this del queue.
        // see LUCENE-9478 for reference
        let prev_max_seq_id = self.next_seq_no.load(Ordering::SeqCst) - 1;
        // Create a new queue with updated parameters
        Ok(DocumentsWriterDeleteQueue::with_params(
            self.info_stream.clone(),
            self.generation + 1,
            seq_no + 1,
            prev_max_seq_id,
        ))
    }

    /// Returns the maximum sequence number for this queue.
    /// This value will change once this queue is advanced.
    pub(crate) fn get_max_seq_no(&self) -> i64 {
        self.max_seq_no.load(Ordering::SeqCst)
    }

    /// Returns `true` if the queue has been advanced.
    pub(crate) fn is_advanced(&self) -> bool {
        let global_state = self.inner.lock();
        global_state.advanced
    }
}

impl Display for DocumentsWriterDeleteQueue {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "DWDP: [ generation: {} ]", self.generation)
    }
}
impl Accountable for DocumentsWriterDeleteQueue {
    fn ram_bytes_used(&self) -> Result<i64> {
        //TODO: memory calculation not implemented
        Ok(0)
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

    pub(crate) fn apply(&mut self, del: &mut MTBufferedUpdates, doc_id_upto: i32) -> Result<()> {
        if Arc::ptr_eq(&self.slice_head, &self.slice_tail) {
            // 0 length slice
            return Ok(());
        }
        // When we apply a slice we take the head and get its next as our first
        //item to apply and continue until we applied the tail. If the head and
        //tail in this slice are not equal then there will be at least one more
        //non-null node in the slice!
        {
            let mut current = self.slice_head.clone();
            loop {
                let next_node_guard = current.next.lock();
                debug_assert!(
                    next_node_guard.is_some(),
                    "slice property violated between the head on the tail must not be a null node"
                );

                next_node_guard.as_ref().unwrap().apply(del, doc_id_upto)?;
                if Arc::ptr_eq(next_node_guard.as_ref().unwrap(), &self.slice_tail) {
                    break;
                }

                let next_node = next_node_guard.as_ref().unwrap().clone();
                drop(next_node_guard);
                current = next_node;
            }
        }
        self.reset();
        Ok(())
    }
    pub(crate) fn reset(&mut self) {
        // Reset to a 0 length slice
        self.slice_head = self.slice_tail.clone();
    }
    /// Returns `true` if the given node is the slice's tail.
    pub(crate) fn is_tail(&self, node: &Arc<Node>) -> bool {
        Arc::ptr_eq(&self.slice_tail, node)
    }

    /// Returns `true` if the item of the given node matches the item in the
    /// tail.
    #[cfg(debug_assertions)]
    pub(crate) fn is_tail_item(&self, item: &NodeEnum) -> bool {
        let node1 = NodeEnum::get_node_base(&self.slice_tail.item);
        let node2 = NodeEnum::get_node_base(item);
        debug_assert!(node1.is_some() && node2.is_some());
        if node1.as_ref().unwrap().item == node2.as_ref().unwrap().item {
            return true;
        }
        false
    }

    pub(crate) fn is_empty(&self) -> bool {
        Arc::ptr_eq(&self.slice_head, &self.slice_tail)
    }
}

/// Represents a node in a linked list.
pub(crate) struct Node {
    /// The next node in the list, or `None` if this is the last node.
    next: Mutex<Option<Arc<Node>>>,
    item: NodeEnum,
}

impl Node {
    pub(crate) fn new(sub_node: NodeEnum) -> Self {
        Self {
            next: Mutex::new(None),
            item: sub_node,
        }
    }
}
impl NodeBase for Node {
    fn apply(&self, buffered_deletes: &mut MTBufferedUpdates, doc_id_upto: i32) -> Result<()> {
        self.item.apply(buffered_deletes, doc_id_upto)
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
    fn apply(&self, _buffered_deletes: &mut MTBufferedUpdates, _doc_id_upto: i32) -> Result<()> {
        Ok(())
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
    fn apply(&self, buffered_deletes: &mut MTBufferedUpdates, doc_id_upto: i32) -> Result<()> {
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
    item: Arc<QueryEnum>,
}
impl QueryNode {
    pub(crate) fn new(query: Arc<QueryEnum>) -> Self {
        Self { item: query }
    }
}
impl NodeBase for QueryNode {
    fn apply(&self, buffered_deletes: &mut MTBufferedUpdates, doc_id_upto: i32) -> Result<()> {
        buffered_deletes.add_query(self.item.clone(), doc_id_upto);
        Ok(())
    }
}
impl Display for QueryNode {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "del={}", self.item)
    }
}
// query node array
pub(crate) struct QueryNodeArray {
    item: Vec<Arc<QueryEnum>>,
}
impl QueryNodeArray {
    pub(crate) fn new(nodes: Vec<Arc<QueryEnum>>) -> Self {
        Self { item: nodes }
    }
}
impl NodeBase for QueryNodeArray {
    fn apply(&self, buffered_deletes: &mut MTBufferedUpdates, doc_id_upto: i32) -> Result<()> {
        for query in &self.item {
            buffered_deletes.add_query(query.clone(), doc_id_upto);
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
    fn apply(&self, buffered_deletes: &mut MTBufferedUpdates, doc_id_upto: i32) -> Result<()> {
        for term in &self.item {
            buffered_deletes.add_term(term, doc_id_upto)?;
        }
        Ok(())
    }
}
impl Display for TermNodeArray {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "del={:?}", self.item)
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
    fn apply(&self, buffered_deletes: &mut MTBufferedUpdates, doc_id_upto: i32) -> Result<()> {
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
}
impl Display for DocValuesUpdatesNode {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let mut sb = String::new();
        sb.push_str("docValuesUpdates: ");
        if !self.item.is_empty() {
            sb.push_str(&format!("term={}; updates: [", self.item[0].term));
            for update in &self.item {
                sb.push_str(&format!(
                    "{}:{},",
                    update.field,
                    update.sub_update.value_to_string()
                ));
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
    QueryNode(QueryNode),
    QueryNodeArray(QueryNodeArray),
    TermNodeArray(TermNodeArray),
    DocValuesUpdatesNode(DocValuesUpdatesNode),
}

impl NodeEnum {
    pub(crate) fn apply(
        &self,
        buffered_deletes: &mut MTBufferedUpdates,
        doc_id_upto: i32,
    ) -> Result<()> {
        match self {
            NodeEnum::TermNode(node) => node.apply(buffered_deletes, doc_id_upto),
            NodeEnum::QueryNode(node) => node.apply(buffered_deletes, doc_id_upto),
            NodeEnum::QueryNodeArray(node) => node.apply(buffered_deletes, doc_id_upto),
            NodeEnum::TermNodeArray(node) => node.apply(buffered_deletes, doc_id_upto),
            NodeEnum::DocValuesUpdatesNode(node) => node.apply(buffered_deletes, doc_id_upto),
            NodeEnum::EmptyNode(node) => node.apply(buffered_deletes, doc_id_upto),
        }
    }
    #[cfg(debug_assertions)]
    pub(crate) fn get_node_base(node: &NodeEnum) -> Option<&TermNodeArray> {
        match node {
            NodeEnum::TermNodeArray(node) => Some(node),
            _ => None,
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
    fn apply(&self, _buffered_deletes: &mut MTBufferedUpdates, _doc_id_upto: i32) -> Result<()> {
        Err(LuceneError::illegal_argument(
            "sentinel item must never be applied",
        ))
    }

    fn is_delete(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicI32, Ordering};
    use std::sync::{Arc, Barrier};
    use std::{thread, vec};

    use parking_lot::Mutex;
    use rand::Rng;

    use crate::core::index::buffered_updates::{
        BufferedUpdates, BufferedUpdatesLock, MAX_INT, MTBufferedUpdates,
    };
    use crate::core::index::doc_values_type::DocValuesType;
    use crate::core::index::doc_values_update::{
        BinaryDocValuesUpdate, DocValuesUpdate, DocValuesUpdateEnum,
    };
    use crate::core::index::documents_writer_delete_queue::{
        DeleteSlice, DocumentsWriterDeleteQueue, NodeEnum, TermNodeArray,
    };
    use crate::core::index::field_term_iterator::FieldTermIterator;

    use crate::core::index::term::Term;
    use crate::core::index::{BytesRef, BytesRefBuilder};

    use crate::core::search::query::Query;
    use crate::core::search::term_query::TermQuery;

    use crate::core::util::bytes_ref_iterator::BytesRefIterator;
    use crate::core::util::error::lucene_error::{LuceneError, Result};
    use crate::core::util::info_stream::get_default_info_stream;
    use crate::test::util::lucene_test_case::lucene_test_case_util::{random, random_multiplier};

    #[allow(dead_code)] // for quick search
    struct TestDocumentsWriterDeleteQueue;

    #[test]
    fn test_update_delete_slices() -> Result<()> {
        let mut random = random();
        let queue = DocumentsWriterDeleteQueue::new(get_default_info_stream());
        let size = 200 + random.random_range(0..500) * random_multiplier();
        let mut ids: Vec<i32> = Vec::with_capacity(size as usize);
        for _ in 0..size {
            ids.push(random.random());
        }
        let mut slice1 = queue.new_slice();
        let mut slice2 = queue.new_slice();
        let mut bd1 = BufferedUpdates::new_sync("bd1");
        let mut bd2 = BufferedUpdates::new_sync("bd2");
        let mut last1 = 0;
        let mut last2 = 0;
        let mut unique_values = HashSet::new();
        for (j, &id) in ids.iter().enumerate() {
            let term = Term::from_text("id", &id.to_string());
            unique_values.insert(term.clone());
            queue.add_delete_term(Vec::from([term.clone()]))?;
            if random.random_range(0..20) == 0 || j == (size - 1) as usize {
                queue.update_slice(&mut slice1)?;
                assert!(
                    slice1.is_tail_item(&NodeEnum::TermNodeArray(TermNodeArray::new(Vec::from([
                        term.clone()
                    ]))))
                );
                slice1.apply(&mut bd1, j as i32)?;
                test_assert_all_between(last1 as i32, j as i32, &mut bd1, &ids)?;
                last1 = j + 1;
            }
            if random.random_range(0..10) == 5 || j == size as usize - 1 {
                queue.update_slice(&mut slice2)?;
                assert!(
                    slice2.is_tail_item(&NodeEnum::TermNodeArray(TermNodeArray::new(Vec::from([
                        term.clone()
                    ]))))
                );
                slice2.apply(&mut bd2, j as i32)?;
                test_assert_all_between(last2 as i32, j as i32, &mut bd2, &ids)?;
                last2 = j + 1;
            }
            let num_deletes = queue.num_global_term_deletes() as usize;
            assert_eq!(unique_values.len(), num_deletes);
        }

        let bd1_terms_set: HashSet<Term> = bd1.delete_terms.key_set();
        let bd2_terms_set: HashSet<Term> = bd2.delete_terms.key_set();
        assert_eq!(unique_values, bd1_terms_set);
        assert_eq!(unique_values, bd2_terms_set);

        let frozen = queue.freeze_global_buffer(&mut None)?.unwrap();
        let mut iter = frozen.delete_terms.iterator();
        let mut frozen_set: HashSet<Term> = HashSet::new();
        let mut bytes_ref = BytesRefBuilder::new();
        while let Some(byte_ref) = iter.next()? {
            bytes_ref.copy_bytes_with_ref(&byte_ref);
            let term = Term::new(iter.field().to_string(), bytes_ref.get_bytes_ref_copy());
            frozen_set.insert(term.clone());
        }
        assert_eq!(unique_values, frozen_set);
        let num_deletes_after = queue.num_global_term_deletes();
        assert_eq!(0, num_deletes_after, "num deletes must be 0 after freeze");

        Ok(())
    }

    fn test_assert_all_between(
        start: i32,
        end: i32,
        deletes: &mut MTBufferedUpdates,
        ids: &[i32],
    ) -> Result<()> {
        for i in start..=end {
            let term = Term::from_text("id", &ids[i as usize].to_string());
            assert_eq!(end, deletes.delete_terms.get(&term));
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
            let term = Term::from_text("id", &i.to_string());
            if random.random_range(0..10) == 0 {
                queue
                    .add_delete_query(Vec::from([Arc::new(TermQuery::new(term.clone()).wrap())]))?;
            } else {
                queue.add_delete_term(vec![term.clone()])?;
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
            let term = Term::from_text("id", &i.to_string());
            if random.random_range(0..10) == 0 {
                queue.add_delete_query(vec![Arc::new(TermQuery::new(term.clone()).wrap())])?;
                queries_since_freeze += 1;
            } else {
                queue.add_delete_term(vec![term.clone()])?;
                terms_since_freeze += 1;
            }

            assert!(queue.any_changes(None));

            if random.random_range(0..5) == 0
                && let Some(frozen) = queue.freeze_global_buffer(&mut None)?
            {
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
        let queue_ = DocumentsWriterDeleteQueue::new(get_default_info_stream());
        let queue = Arc::new(Mutex::new(queue_));
        let lock = queue.lock();
        let handle = thread::spawn({
            let queue = queue.clone();
            move || {
                let term = Term::from_text("foo", "bar");
                queue.lock().add_delete_term(vec![term]).unwrap();
            }
        });
        drop(lock);
        handle.join().unwrap();
        let queue = queue.lock();
        assert!(queue.any_changes(None));
        queue.try_apply_global_slice()?;
        assert!(queue.any_changes(None));
        let frozen_global_buffer_wrap = queue.freeze_global_buffer(&mut None)?;
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
        let ids: Vec<i32> = (0..size).map(|_| random.random()).collect();
        for id in &ids {
            unique_values.insert(Term::from_text("id", &id.to_string()));
        }

        let barrier = Arc::new(Barrier::new(1));
        let index = Arc::new(AtomicI32::new(0));
        let num_threads = 2 + random.random_range(0..5);

        let mut threads = Vec::new();
        for _ in 0..num_threads {
            let thread = UpdateThread::new(
                Arc::clone(&queue),
                Arc::clone(&index),
                ids.clone(),
                Arc::clone(&barrier),
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
        for handle in handles {
            handle.join().expect("Thread join failed");
        }
        for thread in threads {
            let mut guard = thread.lock();
            queue.update_slice(&mut guard.slice)?;
            let deletes = guard.deletes.clone();
            let mut deletes_guard = deletes.lock();
            guard.slice.apply(&mut deletes_guard, MAX_INT)?;
            assert_eq!(unique_values, deletes_guard.delete_terms.key_set());
        }

        queue.try_apply_global_slice()?;
        let mut frozen_set = HashSet::new();
        let frozen = queue.freeze_global_buffer(&mut None)?.unwrap();
        let mut iter = frozen.delete_terms.iterator();
        let mut builder = BytesRefBuilder::new();
        while let Some(byte_ref) = iter.next()? {
            builder.copy_bytes_with_ref(&byte_ref);
            let term = Term::new(iter.field().to_string(), builder.get_bytes_ref_copy());
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
            assert!(matches!(result, Err(LuceneError::AlreadyClosed(_))));
            let result = queue.freeze_global_buffer(&mut None);
            assert!(matches!(result, Err(LuceneError::AlreadyClosed(_))));
            let result = queue.add_delete_query(vec![Arc::new(
                TermQuery::new(Term::from_text("foo", "bar")).wrap(),
            )]);
            assert!(matches!(result, Err(LuceneError::AlreadyClosed(_))));

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
            assert!(matches!(result, Err(LuceneError::AlreadyClosed(_))));
            let result = queue.maybe_freeze_global_buffer()?;
            assert!(result.is_none());
            assert!(!queue.is_open());
        }
        {
            let queue = DocumentsWriterDeleteQueue::new(get_default_info_stream());
            queue.add_delete_term(vec![Term::from_text("foo", "bar")])?;
            let result = queue.close();
            assert!(matches!(result, Err(LuceneError::IllegalState(_))));

            assert!(queue.is_open());
            queue.try_apply_global_slice()?;
            queue.freeze_global_buffer(&mut None)?;
            queue.close()?;
            assert!(!queue.is_open());
        }
        Ok(())
    }

    struct UpdateThread {
        queue: Arc<DocumentsWriterDeleteQueue>,
        index: Arc<AtomicI32>,
        ids: Vec<i32>,
        slice: DeleteSlice,
        deletes: BufferedUpdatesLock,
        barrier: Arc<Barrier>,
    }

    impl UpdateThread {
        fn new(
            queue: Arc<DocumentsWriterDeleteQueue>,
            index: Arc<AtomicI32>,
            ids: Vec<i32>,
            barrier: Arc<Barrier>,
        ) -> Result<Self> {
            let slice = queue.new_slice();
            let deletes = Arc::new(Mutex::new(BufferedUpdates::new_sync("deletes")));

            Ok(UpdateThread {
                queue,
                index,
                ids,
                slice,
                deletes,
                barrier,
            })
        }
        fn run(&mut self) -> Result<()> {
            self.barrier.wait();
            let mut i = 0;
            while i < self.ids.len() {
                let term = Term::from_text("id", &self.ids[i].to_string());
                let term_node = Arc::new(DocumentsWriterDeleteQueue::new_node_with_term(term));
                self.queue
                    .add_with_slice(term_node.clone(), &mut self.slice)?;
                assert!(self.slice.is_tail(&term_node));

                let mut guard = self.deletes.lock();
                self.slice.apply(&mut guard, MAX_INT)?;

                i = self.index.fetch_add(1, Ordering::SeqCst) as usize;
            }
            Ok(())
        }
    }
}
