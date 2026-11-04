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
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::task_executor::TaskExecutor;
use crate::core::util::bit_set::BitSet;
use crate::core::util::bits::{Bits, MatchNoBits};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::hnsw::hnsw_builder::HnswBuilder;
use crate::core::util::hnsw::hnsw_graph::HnswGraph;
use crate::core::util::hnsw::hnsw_graph_builder::{
  HNSW_COMPONENT, HnswGraphBuilder, HnswGraphBuilderBase, HnswGraphBuilderDefaults, rand_seed,
};
use crate::core::util::hnsw::hnsw_graph_searcher::{HnswGraphSearcher, HnswGraphSearcherHook};
use crate::core::util::hnsw::hnsw_lock::HnswLock;
use crate::core::util::hnsw::neighbor_queue::NeighborQueue;
use crate::core::util::hnsw::on_heap_hnsw_graph::OnHeapHnswGraph;
use crate::core::util::hnsw::random_vector_scorer_supplier::RandomVectorScorerSupplier;
use crate::core::util::info_stream::{InfoStream, InfoStreamMT, get_default_info_stream};

/// Number of vectors a worker handles sequentially in one batch.
const DEFAULT_BATCH_SIZE: usize = 2048;

/// A graph builder that manages multiple workers. It only supports adding the
/// whole graph at once. It creates a task for each worker, and the workers pick
/// work in batches.
pub struct HnswConcurrentMergeBuilder<S>
where
  S: RandomVectorScorerSupplier,
{
  task_executor: Arc<TaskExecutor>,
  workers: Vec<ConcurrentMergeWorker<S::RandomVectorScorerSupplier>>,
  info_stream: InfoStreamMT,
  frozen: bool,
}

impl<S> HnswConcurrentMergeBuilder<S>
where
  S: RandomVectorScorerSupplier,
{
  pub fn new(
    task_executor: Arc<TaskExecutor>,
    num_workers: usize,
    scorer_supplier: S,
    m: usize,
    beam_width: usize,
    hnsw: OnHeapHnswGraph,
    initialized_nodes: Option<FixedBitSet>,
  ) -> Result<Self> {
    if num_workers == 0 {
      return Err(LuceneError::illegal_argument("numWorker must be positive"));
    }

    let hnsw = Arc::new(hnsw);
    let hnsw_lock = HnswLock::new();
    let work_progress = Arc::new(AtomicUsize::new(0));
    let initialized_nodes = initialized_nodes.map(Arc::new);
    let info_stream = get_default_info_stream();
    let mut workers = Vec::with_capacity(num_workers);
    for _ in 0..num_workers {
      workers.push(ConcurrentMergeWorker::new(
        scorer_supplier.copy()?,
        m,
        beam_width,
        rand_seed(),
        Arc::clone(&hnsw),
        hnsw_lock.clone(),
        initialized_nodes.clone(),
        Arc::clone(&work_progress),
        DEFAULT_BATCH_SIZE,
        Arc::clone(&info_stream),
      )?);
    }

    Ok(Self {
      task_executor,
      workers,
      info_stream,
      frozen: false,
    })
  }

  fn finish(&mut self) -> Result<()> {
    self.workers.truncate(1);
    self.workers[0].base.finish()
  }

  /// Sets the number of vectors reserved by each worker at a time.
  ///
  /// This is currently exposed for testing only.
  pub fn set_batch_size(&mut self, new_size: usize) -> Result<()> {
    if new_size == 0 {
      return Err(LuceneError::illegal_argument("batchSize must be positive"));
    }
    for worker in &mut self.workers {
      worker.batch_size = new_size;
    }
    Ok(())
  }
}

impl<S> HnswBuilder for HnswConcurrentMergeBuilder<S>
where
  S: RandomVectorScorerSupplier,
{
  fn build(&mut self, max_ord: usize) -> Result<&mut OnHeapHnswGraph> {
    if self.frozen {
      return Err(LuceneError::illegal_state("graph has already been built"));
    }
    if self.info_stream.is_enabled(HNSW_COMPONENT) {
      self.info_stream.message(
        HNSW_COMPONENT,
        &format!(
          "build graph from {max_ord} vectors, with {} workers",
          self.workers.len()
        ),
      )?;
    }

    let tasks = self
      .workers
      .iter_mut()
      .map(|worker| move || worker.run(max_ord));
    self.task_executor.invoke_all(tasks)?;

    self.finish()?;
    self.frozen = true;
    self.workers[0].base.get_completed_graph()
  }

  fn add_graph_node(&mut self, _node: usize) -> Result<()> {
    Err(LuceneError::unsupported_operation(
      "This builder is for merge only",
    ))
  }

  fn set_info_stream(&mut self, info_stream: InfoStreamMT) {
    self.info_stream = info_stream;
    for worker in &mut self.workers {
      worker.base.set_info_stream(Arc::clone(&self.info_stream));
    }
  }

  fn get_graph(&self) -> &OnHeapHnswGraph {
    self.workers[0].base.get_graph()
  }

  fn get_completed_graph(&mut self) -> Result<&mut OnHeapHnswGraph> {
    if !self.frozen {
      // This should already have been called in build(), but just in case.
      self.finish()?;
      self.frozen = true;
    }
    self.workers[0].base.get_completed_graph()
  }
}

pub(crate) struct ConcurrentMergeWorkerHook {
  initialized_nodes: Option<Arc<FixedBitSet>>,
}

impl<B, S, BS, const DEFAULT_SEARCHER: bool> HnswGraphBuilderBase<B, S, BS, DEFAULT_SEARCHER>
  for ConcurrentMergeWorkerHook
where
  B: Bits,
  S: RandomVectorScorerSupplier,
  BS: BitSet,
{
  fn add_graph_node(
    builder: &mut HnswGraphBuilder<B, S, BS, DEFAULT_SEARCHER, Self>,
    node: usize,
  ) -> Result<()> {
    if let Some(initialized_nodes) = builder.hook.initialized_nodes.as_ref()
      && initialized_nodes.get(node)?
    {
      return Ok(());
    }
    HnswGraphBuilderDefaults::add_graph_node(builder, node)
  }
}

struct ConcurrentMergeWorker<S> {
  base: HnswGraphBuilder<MatchNoBits, S, FixedBitSet, false, ConcurrentMergeWorkerHook>,
  work_progress: Arc<AtomicUsize>,
  batch_size: usize,
}

impl<S> ConcurrentMergeWorker<S>
where
  S: RandomVectorScorerSupplier,
{
  #[allow(clippy::too_many_arguments)]
  fn new(
    scorer_supplier: S,
    m: usize,
    beam_width: usize,
    seed: u64,
    hnsw: Arc<OnHeapHnswGraph>,
    hnsw_lock: HnswLock,
    initialized_nodes: Option<Arc<FixedBitSet>>,
    work_progress: Arc<AtomicUsize>,
    batch_size: usize,
    info_stream: InfoStreamMT,
  ) -> Result<Self> {
    let graph_size = hnsw.max_node_id().map_or(0, |node| node + 1);
    let graph_searcher = HnswGraphSearcher::with_hook(
      NeighborQueue::<true>::new(beam_width)?,
      FixedBitSet::new(graph_size),
      HnswGraphSearcherHook::Merge(MergeSearcher::new(hnsw_lock.clone())),
    );
    let hook = ConcurrentMergeWorkerHook { initialized_nodes };
    let mut builder = HnswGraphBuilder::new(
      scorer_supplier,
      m,
      beam_width,
      seed,
      hnsw,
      Some(hnsw_lock),
      graph_searcher,
      hook,
    )?;
    builder.set_info_stream(info_stream);
    Ok(Self {
      base: builder,
      work_progress,
      batch_size,
    })
  }

  /// This method first tries to reserve part of the work by calling
  /// `get_start_pos` and then calls `add_vectors` to actually add the nodes.
  /// This dynamically allocates work to multiple workers so they finish at
  /// approximately the same time.
  fn run(&mut self, max_ord: usize) -> Result<()> {
    let mut start = self.get_start_pos(max_ord);
    while let Some(start_pos) = start {
      let end = max_ord.min(start_pos.saturating_add(self.batch_size));
      self.base.add_vectors_with_range(start_pos, end)?;
      start = self.get_start_pos(max_ord);
    }
    Ok(())
  }

  /// Reserves work by atomically incrementing `work_progress`.
  fn get_start_pos(&self, max_ord: usize) -> Option<usize> {
    let start = self
      .work_progress
      .fetch_add(self.batch_size, Ordering::SeqCst);
    (start < max_ord).then_some(start)
  }
}

/// This searcher obtains the node lock and copies the neighbor array when
/// seeking so concurrent graph modification cannot affect an active search.
pub(crate) struct MergeSearcher {
  hnsw_lock: HnswLock,
  node_buffer: Vec<u32>,
  upto: usize,
}

impl MergeSearcher {
  fn new(hnsw_lock: HnswLock) -> Self {
    Self {
      hnsw_lock,
      node_buffer: Vec::new(),
      upto: 0,
    }
  }
}

impl MergeSearcher {
  pub(crate) fn graph_seek<T>(&mut self, graph: &T, level: usize, target_node: usize) -> Result<()>
  where
    T: HnswGraph,
  {
    let guard = self.hnsw_lock.read(level, target_node);
    let result = graph.with_neighbors(level, target_node, |neighbors| {
      self.node_buffer.clear();
      self
        .node_buffer
        .extend_from_slice(&neighbors.nodes()[..neighbors.size()]);
      Ok(())
    });
    drop(guard);
    result?;
    self.upto = 0;
    Ok(())
  }

  pub(crate) fn graph_next_neighbor<T>(&mut self, _graph: &T) -> Result<usize>
  where
    T: HnswGraph,
  {
    if self.upto < self.node_buffer.len() {
      let node = self.node_buffer[self.upto];
      self.upto += 1;
      Ok(node as usize)
    } else {
      Ok(NO_MORE_DOCS as usize)
    }
  }
}
