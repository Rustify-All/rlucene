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
use std::borrow::Cow;
use std::convert::Infallible;
use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Instant;

use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::knn_collector::KnnCollector;
use crate::core::search::score_doc::ScoreDoc;
use crate::core::search::top_docs::TopDocs;
use crate::core::util::bit_set::BitSet;
use crate::core::util::bits::{Bits, MatchNoBits};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::hnsw::hnsw_builder::HnswBuilder;
use crate::core::util::hnsw::hnsw_graph::HnswGraph;
use crate::core::util::hnsw::hnsw_graph_searcher::HnswGraphSearcher;
use crate::core::util::hnsw::hnsw_lock::HnswLock;
use crate::core::util::hnsw::hnsw_util::HnswUtil;
use crate::core::util::hnsw::neighbor_array::NeighborArray;
use crate::core::util::hnsw::neighbor_queue::NeighborQueue;
use crate::core::util::hnsw::on_heap_hnsw_graph::OnHeapHnswGraph;
use crate::core::util::hnsw::random_vector_scorer::RandomVectorScorer;
use crate::core::util::hnsw::random_vector_scorer_supplier::RandomVectorScorerSupplier;
use crate::core::util::info_stream::{InfoStream, InfoStreamMT, get_default_info_stream};
use crate::core::util::splittable_random::SplittableRandom;
/// Builder for HNSW graph. See [`HnswGraph`] for a gloss on the algorithm and
/// the meaning of the hyper-parameters.
pub struct HnswGraphBuilder<
  B,
  S,
  BS,
  const DEFAULT_SEARCHER: bool = true,
  Hook = HnswGraphBuilderDefaults,
> {
  m: usize,
  ml: f64,
  random: SplittableRandom,
  scorer_supplier: S,
  graph_searcher: HnswGraphSearcher<BS, DEFAULT_SEARCHER>,
  entry_candidates: GraphBuilderKnnCollector,
  beam_candidates: GraphBuilderKnnCollector,
  hnsw: Arc<OnHeapHnswGraph>,
  hnsw_lock: Option<HnswLock>,
  info_stream: InfoStreamMT,
  frozen: bool,
  pub(super) hook: Hook,
  _bits: PhantomData<B>,
}

/// Default node insertion, corresponding to Java's base HnswGraphBuilder.
pub struct HnswGraphBuilderDefaults;

impl<B, S, Hook> HnswGraphBuilder<B, S, FixedBitSet, true, Hook>
where
  S: RandomVectorScorerSupplier,
  B: Bits,
  Hook: HnswGraphBuilderBase<B, S, FixedBitSet, true>,
{
  /// Reads all the vectors from vector values, builds a graph connecting them
  /// by their dense ordinals, using the given hyperparameter settings,
  /// and returns the resulting graph.
  ///
  /// # Arguments
  ///
  /// * `scorer_supplier` - A supplier to create vector scorer from ordinals.
  /// * `m` - Graph fanout parameter used to calculate the maximum number of
  ///   connections a node can have – `M` on upper layers, and `M * 2` on the
  ///   lowest level.
  /// * `beam_width` - The size of the beam search to use when finding nearest
  ///   neighbors.
  /// * `seed` - The seed for a random number generator used during graph
  ///   construction. Provide this to ensure repeatable construction.
  /// * `graph_size` - Size of the graph. If unknown, pass in `-1`.
  pub(crate) fn from_graph_size(
    scorer_supplier: S,
    m: usize,
    beam_width: usize,
    random: u64,
    graph_size: i32,
    hook: Hook,
  ) -> Result<Self> {
    Self::from_hnsw(
      scorer_supplier,
      m,
      beam_width,
      random,
      OnHeapHnswGraph::new(m, graph_size),
      hook,
    )
  }

  pub(crate) fn from_hnsw(
    scorer_supplier: S,
    m: usize,
    beam_width: usize,
    random: u64,
    hnsw: OnHeapHnswGraph,
    hook: Hook,
  ) -> Result<Self> {
    let size = hnsw.size();
    let searcher = HnswGraphSearcher::new(
      NeighborQueue::<true>::new(beam_width)?,
      FixedBitSet::new(size),
    );
    Self::new(
      scorer_supplier,
      m,
      beam_width,
      random,
      Arc::new(hnsw),
      None,
      searcher,
      hook,
    )
  }
}
impl<B, S, BS, const DEFAULT_SEARCHER: bool, Hook>
  HnswGraphBuilder<B, S, BS, DEFAULT_SEARCHER, Hook>
where
  S: RandomVectorScorerSupplier,
  BS: BitSet,
  Hook: HnswGraphBuilderBase<B, S, BS, DEFAULT_SEARCHER>,
  B: Bits,
{
  /// Reads all the vectors from vector values, builds a graph connecting them
  /// by their dense ordinals, using the given hyperparameter settings,
  /// and returns the resulting graph.
  ///
  /// # Arguments
  ///
  /// * `scorer_supplier` - A supplier to create vector scorer from ordinals.
  /// * `m` - Graph fanout parameter used to calculate the maximum number of
  ///   connections a node can have – `M` on upper layers, and `M * 2` on the
  ///   lowest level.
  /// * `beam_width` - The size of the beam search to use when finding nearest
  ///   neighbors.
  /// * `seed` - The seed for a random number generator used during graph
  ///   construction. Provide this to ensure repeatable construction.
  /// * `hnsw` - The graph to build. Can be previously initialized.
  #[allow(clippy::too_many_arguments)]
  pub(crate) fn new(
    scorer_supplier: S,
    m: usize,
    beam_width: usize,
    seed: u64,
    hnsw: Arc<OnHeapHnswGraph>,
    hnsw_lock: Option<HnswLock>,
    graph_searcher: HnswGraphSearcher<BS, DEFAULT_SEARCHER>,
    hook: Hook,
  ) -> Result<Self> {
    if m == 0 {
      return Err(LuceneError::illegal_argument(
        "M (max connections) must be positive",
      ));
    }
    if beam_width == 0 {
      return Err(LuceneError::illegal_argument("beamWidth must be positive"));
    }

    let ml = if m == 1 { 1.0 } else { 1.0 / (m as f64).ln() };

    let random = SplittableRandom::new(seed);

    Ok(Self {
      m,
      ml,
      random,
      scorer_supplier,
      hnsw,
      hnsw_lock,
      graph_searcher,
      entry_candidates: GraphBuilderKnnCollector::new(1)?,
      beam_candidates: GraphBuilderKnnCollector::new(beam_width)?,
      info_stream: get_default_info_stream(),
      frozen: false,
      hook,
      _bits: PhantomData,
    })
  }
  /// add vectors in range [minOrd, maxOrd)
  pub(crate) fn add_vectors_with_range(&mut self, min_ord: usize, max_ord: usize) -> Result<()> {
    if self.frozen {
      return Err(LuceneError::illegal_state(
        "This HnswGraphBuilder is frozen and cannot be updated",
      ));
    }

    let start = Instant::now();
    let mut last_log = start;

    if self.info_stream.is_enabled(HNSW_COMPONENT) {
      self
        .info_stream
        .message(HNSW_COMPONENT, &format!("addVectors [{min_ord} {max_ord})"))?;
    }

    for node in min_ord..max_ord {
      self.add_graph_node(node)?;
      if node % 10_000 == 0 && self.info_stream.is_enabled(HNSW_COMPONENT) {
        last_log = self.print_graph_build_status(node, start, last_log)?;
      }
    }

    Ok(())
  }
  fn add_vectors(&mut self, max_ord: usize) -> Result<()> {
    self.add_vectors_with_range(0, max_ord)
  }
  fn print_graph_build_status(&self, node: usize, start: Instant, t: Instant) -> Result<Instant> {
    let now = Instant::now();
    if self.info_stream.is_enabled(HNSW_COMPONENT) {
      let elapsed_t = now.duration_since(t).as_millis();
      let elapsed_start = now.duration_since(start).as_millis();
      self.info_stream.message(
        HNSW_COMPONENT,
        &format!("built {node} in {elapsed_t}/{elapsed_start} ms"),
      )?;
    }
    Ok(now)
  }
  pub(crate) fn finish(&mut self) -> Result<()> {
    self.connect_components()?;
    self.frozen = true;
    Ok(())
  }
  fn connect_components(&mut self) -> Result<()> {
    let start = Instant::now();

    for level in 0..self.hnsw.num_levels()? {
      match self.connect_components_with_level(level) {
        Ok(false) if self.info_stream.is_enabled(HNSW_COMPONENT) => {
          self.info_stream.message(
            HNSW_COMPONENT,
            &format!("connectComponents failed on level {level}"),
          )?;
        },
        Err(e) => return Err(e),
        _ => {},
      }
    }

    if self.info_stream.is_enabled(HNSW_COMPONENT) {
      let elapsed = start.elapsed().as_millis();
      self
        .info_stream
        .message(HNSW_COMPONENT, &format!("connectComponents {elapsed} ms"))?;
    }

    Ok(())
  }

  fn connect_components_with_level(&mut self, level: usize) -> Result<bool> {
    debug_assert!(self.hnsw.size() <= i32::MAX as usize);
    let mut not_fully_connected = FixedBitSet::new(self.hnsw.size());
    let mut max_conn = self.m;
    if level == 0 {
      max_conn *= 2;
    }

    let components = HnswUtil::components(
      &mut self.hnsw,
      level,
      Some(&mut not_fully_connected),
      max_conn,
    )?;

    if self.info_stream.is_enabled(HNSW_COMPONENT) {
      self.info_stream.message(
        HNSW_COMPONENT,
        &format!("connect {} components on level={}", components.len(), level),
      )?;
    }

    let mut result = true;

    if components.len() > 1 {
      let mut c0_index = 0;
      for index in 1..components.len() {
        if components[index].size > components[c0_index].size {
          c0_index = index;
        }
      }
      let c0 = &components[c0_index];

      if c0.start == NO_MORE_DOCS as usize {
        // the component is already fully connected - no room for new connections
        return Ok(false);
      }
      // try for more connections? We only do one since otherwise they may become full
      // while linking
      let mut beam = GraphBuilderKnnCollector::new(2)?;
      let mut eps = [c0.start];
      #[allow(clippy::needless_range_loop)]
      for index in 0..components.len() {
        let c = &components[index];
        if index == c0_index || c.start == NO_MORE_DOCS as usize {
          continue;
        }

        if self.info_stream.is_enabled(HNSW_COMPONENT) {
          self.info_stream.message(
            HNSW_COMPONENT,
            &format!("connect component {c:?} to {c0:?}"),
          )?;
        }

        beam.clear();
        eps[0] = c0.start;
        let scorer = self.scorer_supplier.scorer(c.start)?;
        // find the closest node in the largest component to the lowest-numbered node in
        // this component that has room to make a connection
        self.graph_searcher.search_level_with_collector(
          &mut beam,
          &scorer,
          level,
          &eps,
          &mut self.hnsw,
          &Some(&not_fully_connected),
        )?;

        let mut linked = false;

        while beam.size() > 0 {
          let c0node = beam.pop_node()?;
          if c0node == c.start || !not_fully_connected.get(c0node)? {
            continue;
          }

          let score = beam.minimum_score();
          debug_assert!(not_fully_connected.get(c0node)?);
          // link the nodes
          Self::link(
            self.hnsw.as_ref(),
            level,
            c0node,
            c.start,
            score,
            &mut not_fully_connected,
          )?;

          linked = true;

          if self.info_stream.is_enabled(HNSW_COMPONENT) {
            self.info_stream.message(
              HNSW_COMPONENT,
              &format!("connected ok {} -> {}", c0node, c.start),
            )?;
          }
        }

        if !linked {
          if self.info_stream.is_enabled(HNSW_COMPONENT) {
            self
              .info_stream
              .message(HNSW_COMPONENT, "not connected; no free nodes found")?;
          }
          result = false;
        }
      }
    }

    Ok(result)
  }
  // Try to link two nodes bidirectionally; the forward connection will always be
  // made. Update notFullyConnected.
  fn link(
    hnsw: &OnHeapHnswGraph,
    level: usize,
    n0: usize,
    n1: usize,
    score: f32,
    not_fully_connected: &mut FixedBitSet,
  ) -> Result<()> {
    let max_conn = hnsw.with_neighbors_mut(level, n0, |nbr0| {
      // must subtract 1 here since the nodes array is one larger than the configured
      // max neighbors (M / 2M).
      // We should have taken care of this check by searching for not-full nodes
      let max_conn = nbr0.nodes().len() - 1;

      debug_assert!(not_fully_connected.get(n0)?);
      debug_assert!(
        nbr0.size() < max_conn,
        "node {} is full, has {} friends",
        n0,
        nbr0.size()
      );

      nbr0.add_out_of_order(n1, score)?;

      if nbr0.size() == max_conn {
        not_fully_connected.clear_with_index(n0)?;
      }
      Ok(max_conn)
    })?;

    hnsw.with_neighbors_mut(level, n1, |nbr1| {
      if nbr1.size() < max_conn {
        nbr1.add_out_of_order(n0, score)?;
        if nbr1.size() == max_conn {
          not_fully_connected.clear_with_index(n1)?;
        }
      }
      Ok(())
    })
  }
  pub(crate) fn get_scorer_supplier_mut(&mut self) -> &mut S {
    &mut self.scorer_supplier
  }
  pub(crate) fn get_scorer_supplier(&self) -> &S {
    &self.scorer_supplier
  }
}

impl<B, S, BS, const DEFAULT_SEARCHER: bool, Hook> HnswBuilder
  for HnswGraphBuilder<B, S, BS, DEFAULT_SEARCHER, Hook>
where
  S: RandomVectorScorerSupplier,
  BS: BitSet,
  Hook: HnswGraphBuilderBase<B, S, BS, DEFAULT_SEARCHER>,
  B: Bits,
{
  fn build(&mut self, max_ord: usize) -> Result<&mut OnHeapHnswGraph> {
    if self.frozen {
      return Err(LuceneError::illegal_state(
        "This HnswGraphBuilder is frozen and cannot be updated",
      ));
    }

    if self.info_stream.is_enabled(HNSW_COMPONENT) {
      self.info_stream.message(
        HNSW_COMPONENT,
        &format!("build graph from {max_ord} vectors"),
      )?;
    }

    self.add_vectors(max_ord)?;
    self.get_completed_graph()
  }

  fn add_graph_node(&mut self, node: usize) -> Result<()> {
    Hook::add_graph_node(self, node)
  }

  fn set_info_stream(&mut self, info_stream: InfoStreamMT) {
    self.info_stream = info_stream;
  }

  fn get_graph(&self) -> &OnHeapHnswGraph {
    self.hnsw.as_ref()
  }

  fn get_completed_graph(&mut self) -> Result<&mut OnHeapHnswGraph> {
    if !self.frozen {
      self.finish()?;
    }
    Arc::get_mut(&mut self.hnsw)
      .ok_or_else(|| LuceneError::illegal_state("concurrent graph is still shared"))
  }
}

impl HnswGraphBuilderDefaults {
  fn add_diverse_neighbors<T>(
    hnsw: &OnHeapHnswGraph,
    scorer_supplier: &T,
    m: usize,
    hnsw_lock: Option<&HnswLock>,
    level: usize,
    node: usize,
    candidates: &NeighborArray<false>,
  ) -> Result<()>
  where
    T: RandomVectorScorerSupplier,
  {
    let max_conn_on_level = if level == 0 { m * 2 } else { m };
    /* For each of the beamWidth nearest candidates (going from best to worst),
     * select it only if it is closer to target than it is to any of the
     * already-selected neighbors (ie selected in this method,
     * since the node is new and has no prior neighbors).
     */
    let mut mask_buffer = [false; 128];
    let mask = Self::select_and_link_diverse(
      hnsw,
      scorer_supplier,
      candidates,
      max_conn_on_level,
      level,
      node,
      &mut mask_buffer,
    )?;

    // Link the selected nodes to the new node, and the new node to the selected
    // nodes (again applying diversity heuristic)
    // NOTE: here we're using candidates and mask but not the neighbour array
    // because once we have added incoming link there will be possibilities
    // of this node being discovered and neighbour array being modified. So
    // using local candidates and mask is a safer option.
    #[allow(clippy::needless_range_loop)]
    for i in 0..candidates.size() {
      if !mask[i] {
        continue;
      }
      let nbr = candidates.nodes()[i] as usize;
      let score = candidates.scores()[i];
      let guard = hnsw_lock.map(|lock| lock.write(level, nbr));
      hnsw.with_neighbors_mut(level, nbr, |neighbors| {
        neighbors.add_and_ensure_diversity(node, score, nbr, scorer_supplier)
      })?;
      drop(guard);
    }

    Ok(())
  }
  ///  This method will select neighbors to add and return a mask telling the
  /// caller which candidates are selected
  fn select_and_link_diverse<'a, T>(
    hnsw: &OnHeapHnswGraph,
    scorer_supplier: &T,
    candidates: &NeighborArray<false>,
    max_conn_on_level: usize,
    level: usize,
    node: usize,
    mask_buffer: &'a mut [bool; 128],
  ) -> Result<Cow<'a, [bool]>>
  where
    T: RandomVectorScorerSupplier,
  {
    let max_node_id = hnsw.max_node_id();
    hnsw.with_neighbors_mut(level, node, |neighbors| {
      debug_assert_eq!(neighbors.size(), 0); // new node
      let mut heap_mask = Vec::new();
      let mask = if candidates.size() <= mask_buffer.len() {
        &mut mask_buffer[..candidates.size()]
      } else {
        heap_mask = vec![false; candidates.size()];
        &mut heap_mask
      };
      let mut i = candidates.size();
      // Select the best maxConnOnLevel neighbors of the new node, applying the
      // diversity heuristic
      while neighbors.size() < max_conn_on_level && i > 0 {
        i -= 1;
        // compare each neighbor (in distance order) against the closer neighbors
        // selected so far, only adding it if it is closer to the target
        // than to any of the other selected neighbors
        let c_node = candidates.nodes()[i] as usize;
        let c_score = candidates.scores()[i];
        debug_assert!({
          match max_node_id {
            Some(v) => c_node <= v,
            None => false,
          }
        });
        let mut v = scorer_supplier.scorer(c_node)?;
        if Self::diversity_check(c_score, &mut v, &neighbors.nodes()[..neighbors.size()])? {
          mask[i] = true;
          // here we don't need to lock, because there's no incoming link so no others is
          // able to discover this node such that no others will modify
          // this neighbor array as well
          neighbors.add_in_order(c_node, c_score)?;
        }
      }
      if candidates.size() <= mask_buffer.len() {
        Ok(Cow::Borrowed(&mask_buffer[..candidates.size()]))
      } else {
        Ok(Cow::Owned(heap_mask))
      }
    })
  }
  pub(crate) fn pop_to_scratch(
    candidates: &mut GraphBuilderKnnCollector,
    scratch: &mut NeighborArray<false>,
  ) -> Result<()> {
    scratch.clear();
    let candidate_count = candidates.size();

    for _ in 0..candidate_count {
      let max_similarity = candidates.minimum_score();
      let node = candidates.pop_node()?;
      scratch.add_in_order(node, max_similarity)?;
    }

    Ok(())
  }
  /// # Arguments
  ///
  /// * `candidate` - The vector of a new candidate neighbor of a node `n`.
  /// * `score` - The score of the new candidate and node `n`, to be compared
  ///   with scores of the candidate and `n`'s neighbors.
  /// * `neighbors` - The neighbors selected so far.
  ///
  /// # Returns
  ///
  /// Whether the candidate is diverse given the existing neighbors.
  #[inline]
  fn diversity_check<T>(score: f32, scorer: &mut T, neighbors: &[u32]) -> Result<bool>
  where
    T: RandomVectorScorer,
  {
    for &neighbor in neighbors {
      let neighbor_similarity = scorer.score(neighbor as usize)?;
      if neighbor_similarity >= score {
        return Ok(false);
      }
    }
    Ok(true)
  }
  pub(crate) fn get_random_graph_level(ml: f64, random: &mut SplittableRandom) -> usize {
    loop {
      let rand_double = random.next_double();
      if rand_double > 0.0 {
        return (-rand_double.ln() * ml) as usize;
      }
    }
  }

  pub(crate) fn add_graph_node<B, S, BS, const DEFAULT_SEARCHER: bool, Hook>(
    builder: &mut HnswGraphBuilder<B, S, BS, DEFAULT_SEARCHER, Hook>,
    node: usize,
  ) -> Result<()>
  where
    B: Bits,
    S: RandomVectorScorerSupplier,
    BS: BitSet,
    Hook: HnswGraphBuilderBase<B, S, BS, DEFAULT_SEARCHER>,
  {
    /*
    Note: this implementation is thread safe when graph size is fixed (e.g. when merging)
    The process of adding a node is roughly:
    1. Add the node to all level from top to the bottom, but do not connect it to any other node,
       nor try to promote itself to an entry node before the connection is done. (Unless the graph is empty
       and this is the first node, in that case we set the entry node and return)
    2. Do the search from top to bottom, remember all the possible neighbours on each level the node
       is on.
    3. Add the neighbor to the node from bottom to top level, when adding the neighbour,
       we always add all the outgoing links first before adding incoming link such that
       when a search visits this node, it can always find a way out
    4. If the node has level that is less or equal to graph level, then we're done here.
       If the node has level larger than graph level, then we need to promote the node
       as the entry node. If, while we add the node to the graph, the entry node has changed
       (which means the graph level has changed as well), we need to reinsert the node
       to the newly introduced levels (repeating step 2,3 for new levels) and again try to
       promote the node to entry node.
     */

    if builder.frozen {
      return Err(LuceneError::illegal_state(
        "Graph builder is already frozen",
      ));
    }

    let scorer = builder.scorer_supplier.scorer(node)?;
    let node_level = Self::get_random_graph_level(builder.ml, &mut builder.random);

    // first add nodes to all levels
    for level in (0..=node_level).rev() {
      if let Some(hnsw) = Arc::get_mut(&mut builder.hnsw) {
        hnsw.add_node(level, node)?;
      } else {
        builder.hnsw.add_node_without_growing(level, node)?;
      }
    }
    // then promote itself as entry node if entry node is not set
    if builder.hnsw.try_set_new_entry_node(node, node_level) {
      return Ok(());
    }

    // if the entry node is already set, then we have to do all connections first
    // before we can promote ourselves as entry node
    let mut lowest_unset_level = 0;
    let mut cur_max_level;
    loop {
      let mut eps = {
        cur_max_level = builder.hnsw.num_levels()? - 1;
        // NOTE: the entry node and max level may not be paired, but because we get the
        // level first we ensure that the entry node we get later will
        // always exist on the curMaxLevel
        match builder.hnsw.entry_node()? {
          Some(v) => [v],
          None => {
            return Err(LuceneError::illegal_state(
              "Entry node is not set when trying to add connections",
            ));
          },
        }
      };
      let top = std::cmp::min(node_level, cur_max_level);
      let mut scratch_per_level: smallvec::SmallVec<[NeighborArray<false>; 1]> =
        smallvec::smallvec![NeighborArray::<false>::default(); top - lowest_unset_level + 1];
      let candidates = &mut builder.entry_candidates;
      for level in (node_level + 1..=cur_max_level).rev() {
        candidates.clear();
        if let Some(hnsw) = Arc::get_mut(&mut builder.hnsw) {
          builder.graph_searcher.search_level_with_collector(
            candidates,
            &scorer,
            level,
            &eps,
            hnsw,
            &None::<Infallible>,
          )?;
        } else {
          builder.graph_searcher.search_level_with_collector(
            candidates,
            &scorer,
            level,
            &eps,
            builder.hnsw.as_ref(),
            &None::<Infallible>,
          )?;
        }
        eps[0] = candidates.pop_node()?;
      }

      // for levels <= nodeLevel search with topk = beamWidth, and add connections
      let mut eps: Cow<'_, [usize]> = Cow::Borrowed(&eps);
      let candidates = &mut builder.beam_candidates;
      for i in (0..scratch_per_level.len()).rev() {
        let level = i + lowest_unset_level;
        candidates.clear();
        if let Some(hnsw) = Arc::get_mut(&mut builder.hnsw) {
          builder.graph_searcher.search_level_with_collector(
            candidates,
            &scorer,
            level,
            &eps,
            hnsw,
            &None::<Infallible>,
          )?;
        } else {
          builder.graph_searcher.search_level_with_collector(
            candidates,
            &scorer,
            level,
            &eps,
            builder.hnsw.as_ref(),
            &None::<Infallible>,
          )?;
        }
        eps = if i == 0 {
          // This pass has no lower search level that consumes another entry-point snapshot.
          candidates.trim_to_nearest_k_nodes()?;
          Cow::Borrowed(&[])
        } else {
          Cow::Owned(candidates.pop_until_nearest_k_nodes()?)
        };
        let mut scratch = NeighborArray::<false>::new(candidates.size());
        Self::pop_to_scratch(candidates, &mut scratch)?;
        scratch_per_level[i] = scratch;
      }

      // then do connections from bottom up
      let len = scratch_per_level.len();
      for (i, scratch) in scratch_per_level.into_iter().enumerate() {
        Self::add_diverse_neighbors(
          builder.hnsw.as_ref(),
          &builder.scorer_supplier,
          builder.m,
          builder.hnsw_lock.as_ref(),
          i + lowest_unset_level,
          node,
          &scratch,
        )?;
      }

      lowest_unset_level += len;
      debug_assert!(lowest_unset_level == (std::cmp::min(cur_max_level, node_level) + 1));
      if lowest_unset_level > node_level {
        return Ok(());
      }

      debug_assert!(lowest_unset_level == (cur_max_level + 1) && node_level > cur_max_level);
      if builder
        .hnsw
        .try_promote_new_entry_node(node, node_level, cur_max_level)
      {
        return Ok(());
      }

      if builder.hnsw.num_levels()? == cur_max_level + 1 {
        // This should never happen if all the calculations are correct
        return Err(LuceneError::illegal_state(format!(
          "Unable to promote node {node} at level {node_level} as entry. Graph level {cur_max_level} has not changed."
        )));
      }
    }
  }
}

/// Node insertion behavior selected by the existing Java builder class.
pub trait HnswGraphBuilderBase<B, S, BS, const DEFAULT_SEARCHER: bool>: Sized
where
  B: Bits,
  S: RandomVectorScorerSupplier,
  BS: BitSet,
{
  fn add_graph_node(
    builder: &mut HnswGraphBuilder<B, S, BS, DEFAULT_SEARCHER, Self>,
    node: usize,
  ) -> Result<()>;
}

impl<B, S, BS, const DEFAULT_SEARCHER: bool> HnswGraphBuilderBase<B, S, BS, DEFAULT_SEARCHER>
  for HnswGraphBuilderDefaults
where
  B: Bits,
  S: RandomVectorScorerSupplier,
  BS: BitSet,
{
  #[inline]
  fn add_graph_node(
    builder: &mut HnswGraphBuilder<B, S, BS, DEFAULT_SEARCHER, Self>,
    node: usize,
  ) -> Result<()> {
    HnswGraphBuilderDefaults::add_graph_node(builder, node)
  }
}

/// A restricted, specialized [`KnnCollector`] that can be used when building a
/// graph.
///
/// This collector does **not** support [`TopDocs`].
pub struct GraphBuilderKnnCollector {
  queue: NeighborQueue<false>,
  k: usize,
  visited_count: usize,
}
impl GraphBuilderKnnCollector {
  pub fn new(k: usize) -> Result<Self> {
    Ok(Self {
      queue: NeighborQueue::<false>::new(k)?,
      k,
      visited_count: 0,
    })
  }

  pub fn size(&self) -> usize {
    self.queue.size()
  }

  pub fn pop_node(&mut self) -> Result<usize> {
    self.queue.pop()
  }

  pub fn pop_until_nearest_k_nodes(&mut self) -> Result<Vec<usize>> {
    self.trim_to_nearest_k_nodes()?;
    Ok(self.queue.nodes())
  }

  #[inline]
  fn trim_to_nearest_k_nodes(&mut self) -> Result<()> {
    while self.size() > self.k {
      self.queue.pop()?;
    }
    Ok(())
  }

  pub fn minimum_score(&self) -> f32 {
    self.queue.top_score()
  }

  pub fn clear(&mut self) {
    self.queue.clear();
    self.visited_count = 0;
  }
}
impl KnnCollector for GraphBuilderKnnCollector {
  fn early_terminated(&self) -> bool {
    false
  }

  fn inc_visited_count(&mut self, count: usize) {
    self.visited_count += count;
  }

  fn visited_count(&self) -> usize {
    self.visited_count
  }

  fn visit_limit(&self) -> usize {
    i64::MAX as usize
  }

  fn k(&self) -> usize {
    self.k
  }

  fn collect(&mut self, doc_id: usize, similarity: f32) -> Result<bool> {
    self.queue.insert_with_overflow(doc_id, similarity)
  }

  fn min_competitive_similarity(&self) -> Result<f32> {
    if self.queue.size() >= self.k {
      Ok(self.queue.top_score())
    } else {
      Ok(f32::NEG_INFINITY)
    }
  }

  fn top_docs(&mut self) -> Result<TopDocs<ScoreDoc>> {
    Err(LuceneError::illegal_state(""))
  }
}

/// Default number of maximum connections per node.
pub const DEFAULT_MAX_CONN: usize = 16;

/// Default size of the queue maintained during graph construction.
pub const DEFAULT_BEAM_WIDTH: usize = 100;

/// Default random seed for level generation.
pub const DEFAULT_RAND_SEED: u64 = 42;

/// Name for the HNSW component used in the info stream.
pub const HNSW_COMPONENT: &str = "HNSW";

/// Random seed for level generation; public to expose for testing *
pub const RAND_SEED: u64 = DEFAULT_RAND_SEED;

#[cfg(test)]
std::thread_local! {
  static TEST_RAND_SEED: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

pub(crate) fn rand_seed() -> u64 {
  #[cfg(test)]
  if let Some(seed) = TEST_RAND_SEED.get() {
    return seed;
  }
  RAND_SEED
}

#[cfg(test)]
pub(crate) struct TestRandSeedGuard {
  previous: Option<u64>,
}

#[cfg(test)]
impl TestRandSeedGuard {
  pub(crate) fn new(seed: u64) -> Self {
    let previous = TEST_RAND_SEED.replace(Some(seed));
    Self { previous }
  }
}

#[cfg(test)]
impl Drop for TestRandSeedGuard {
  fn drop(&mut self) {
    TEST_RAND_SEED.set(self.previous);
  }
}

pub fn create<S>(
  scorer_supplier: S,
  m: usize,
  beam_width: usize,
  random: u64,
) -> Result<DefaultHnswGraphBuilder<S>>
where
  S: RandomVectorScorerSupplier,
{
  HnswGraphBuilder::from_graph_size(
    scorer_supplier,
    m,
    beam_width,
    random,
    -1,
    HnswGraphBuilderDefaults,
  )
}

/// Equivalent to `HnswGraphBuilder::create(scorerSupplier, M, beamWidth,
/// seed, graphSize)`
pub fn create_with_graph_size<S>(
  scorer_supplier: S,
  m: usize,
  beam_width: usize,
  random: u64,
  graph_size: i32,
) -> Result<DefaultHnswGraphBuilder<S>>
where
  S: RandomVectorScorerSupplier,
{
  HnswGraphBuilder::from_graph_size(
    scorer_supplier,
    m,
    beam_width,
    random,
    graph_size,
    HnswGraphBuilderDefaults,
  )
}
pub type DefaultHnswGraphBuilder<S> = HnswGraphBuilder<MatchNoBits, S, FixedBitSet>;
