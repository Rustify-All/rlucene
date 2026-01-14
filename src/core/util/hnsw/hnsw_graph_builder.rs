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
use std::time::Instant;

use rand_chacha::ChaCha20Rng;
use rand_chacha::rand_core::RngCore;

use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::search::dummy::dummy_score_doc_like::DummyScoreDocLike;
use crate::core::search::knn_collector::KnnCollector;
use crate::core::search::top_docs::TopDocs;
use crate::core::util::bit_set::BitSet;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::hnsw::hnsw_builder::HnswBuilder;
use crate::core::util::hnsw::hnsw_graph::{HnswGraph, HnswGraphEnums};
use crate::core::util::hnsw::hnsw_graph_searcher::{
    HnswGraphSearcher, HnswGraphSearcherBase, HnswGraphSearcherBaseDefault,
};
use crate::core::util::hnsw::hnsw_lock::HnswLock;
use crate::core::util::hnsw::hnsw_util::HnswUtil;
use crate::core::util::hnsw::neighbor_array::NeighborArray;
use crate::core::util::hnsw::neighbor_queue::NeighborQueue;
use crate::core::util::hnsw::on_heap_hnsw_graph::OnHeapHnswGraph;
use crate::core::util::hnsw::random_vector_scorer::RandomVectorScorer;
use crate::core::util::hnsw::random_vector_scorer_supplier::RandomVectorScorerSupplier;
use crate::core::util::info_stream::{InfoStream, InfoStreamEnum, InfoStreamMT, NoOutput};
/// Builder for HNSW graph. See [`HnswGraph`] for a gloss on the algorithm and
/// the meaning of the hyper-parameters.
pub struct HnswGraphBuilder<S, B, H>
where
    S: RandomVectorScorerSupplier,
    B: BitSet,
    H: HnswGraphSearcherBase,
{
    m: usize,
    ml: f64,
    random: ChaCha20Rng,
    scorer_supplier: S,
    graph_searcher: HnswGraphSearcher<B, H>,
    entry_candidates: GraphBuilderKnnCollector,
    beam_candidates: GraphBuilderKnnCollector,
    hnsw: HnswGraphEnums,
    hnsw_lock: Option<HnswLock>,
    info_stream: InfoStreamMT,
    frozen: bool,
}
impl<S> HnswGraphBuilder<S, FixedBitSet, HnswGraphSearcherBaseDefault>
where
    S: RandomVectorScorerSupplier,
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
        random: ChaCha20Rng,
        graph_size: i32,
    ) -> Result<Self> {
        let hnsw = HnswGraphEnums::OnHeap(OnHeapHnswGraph::new(m, graph_size));
        Self::from_hnsw(scorer_supplier, m, beam_width, random, hnsw)
    }

    pub fn from_hnsw(
        scorer_supplier: S,
        m: usize,
        beam_width: usize,
        random: ChaCha20Rng,
        hnsw: HnswGraphEnums,
    ) -> Result<Self> {
        let size = hnsw.size();
        let searcher = HnswGraphSearcher::new(
            NeighborQueue::new(beam_width, true)?,
            FixedBitSet::new(size),
            HnswGraphSearcherBaseDefault,
        );
        Self::new(scorer_supplier, m, beam_width, random, hnsw, None, searcher)
    }
}
impl<S, B, H> HnswGraphBuilder<S, B, H>
where
    S: RandomVectorScorerSupplier,
    B: BitSet,
    H: HnswGraphSearcherBase,
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
    pub fn new(
        scorer_supplier: S,
        m: usize,
        beam_width: usize,
        random: ChaCha20Rng,
        hnsw: HnswGraphEnums,
        hnsw_lock: Option<HnswLock>,
        graph_searcher: HnswGraphSearcher<B, H>,
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
            info_stream: Arc::new(InfoStreamEnum::NoOutput(NoOutput)),
            frozen: false,
        })
    }
    /// add vectors in range [minOrd, maxOrd)
    pub(crate) fn add_vectors(&mut self, min_ord: usize, max_ord: usize) -> Result<()> {
        if self.frozen {
            return Err(LuceneError::illegal_state(
                "This HnswGraphBuilder is frozen and cannot be updated",
            ));
        }

        let start = Instant::now();
        let mut last_log = start;

        if self.info_stream.enabled(HNSW_COMPONENT) {
            self.info_stream
                .message(HNSW_COMPONENT, &format!("addVectors [{min_ord} {max_ord})"));
        }

        for node in min_ord..max_ord {
            self.add_graph_node(node)?;
            if node % 10_000 == 0 && self.info_stream.enabled(HNSW_COMPONENT) {
                last_log = self.print_graph_build_status(node, start, last_log);
            }
        }

        Ok(())
    }
    fn add_all_vectors(&mut self, max_ord: usize) -> Result<()> {
        self.add_vectors(0, max_ord)
    }
    fn print_graph_build_status(&mut self, node: usize, start: Instant, t: Instant) -> Instant {
        let now = Instant::now();
        if self.info_stream.enabled(HNSW_COMPONENT) {
            let elapsed_t = now.duration_since(t).as_millis();
            let elapsed_start = now.duration_since(start).as_millis();
            self.info_stream.message(
                HNSW_COMPONENT,
                &format!("built {node} in {elapsed_t}/{elapsed_start} ms"),
            );
        }
        now
    }
    fn add_diverse_neighbors(
        &mut self,
        level: usize,
        node: usize,
        candidates: &NeighborArray,
    ) -> Result<()> {
        let HnswGraphEnums::OnHeap(hnsw) = &mut self.hnsw;
        /* For each of the beamWidth nearest candidates (going from best to worst),
         * select it only if it is closer to target than it is to any of the
         * already-selected neighbors (ie selected in this method,
         * since the node is new and has no prior neighbors).
         */
        let neighbors = hnsw.get_neighbors(level, node);
        debug_assert_eq!(neighbors.size(), 0); // new node
        let max_conn_on_level = if level == 0 { self.m * 2 } else { self.m };
        let mask = Self::select_and_link_diverse(candidates, max_conn_on_level, self, level, node)?;

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
            let nbr = candidates.nodes()[i];
            let score = candidates.scores()[i];

            if let Some(lock) = &self.hnsw_lock {
                let guard = lock.write(level, nbr);
                NeighborArray::add_and_ensure_diversity(
                    &mut self.hnsw,
                    level,
                    node,
                    score,
                    nbr,
                    &self.scorer_supplier,
                )?;
                drop(guard);
            } else {
                NeighborArray::add_and_ensure_diversity(
                    &mut self.hnsw,
                    level,
                    node,
                    score,
                    nbr,
                    &self.scorer_supplier,
                )?;
            }
        }

        Ok(())
    }
    ///  This method will select neighbors to add and return a mask telling the
    /// caller which candidates are selected
    fn select_and_link_diverse(
        candidates: &NeighborArray,
        max_conn_on_level: usize,
        builder: &mut HnswGraphBuilder<S, B, H>,
        level: usize,
        node: usize,
    ) -> Result<Vec<bool>> {
        let mut mask = vec![false; candidates.size()];
        let mut i = candidates.size();
        let HnswGraphEnums::OnHeap(hnsw) = &mut builder.hnsw;
        let max_node_id = hnsw.max_node_id();
        let neighbors = hnsw.get_neighbors(level, node);
        // Select the best maxConnOnLevel neighbors of the new node, applying the
        // diversity heuristic
        while neighbors.size() < max_conn_on_level && i > 0 {
            i -= 1;
            // compare each neighbor (in distance order) against the closer neighbors
            // selected so far, only adding it if it is closer to the target
            // than to any of the other selected neighbors
            let c_node = candidates.nodes()[i];
            let c_score = candidates.scores()[i];
            debug_assert!({
                match max_node_id {
                    Some(v) => c_node <= v,
                    None => false,
                }
            });
            let v = builder.scorer_supplier.scorer(c_node)?;
            if Self::diversity_check(c_score, &v, neighbors)? {
                mask[i] = true;
                // here we don't need to lock, because there's no incoming link so no others is
                // able to discover this node such that no others will modify
                // this neighbor array as well
                neighbors.add_in_order(c_node, c_score)?;
            }
        }

        Ok(mask)
    }
    fn pop_to_scratch(
        candidates: &mut GraphBuilderKnnCollector,
        scratch: &mut NeighborArray,
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
    fn diversity_check(
        score: f32,
        scorer: &impl RandomVectorScorer,
        neighbors: &NeighborArray,
    ) -> Result<bool> {
        for i in 0..neighbors.size() {
            let neighbor_similarity = scorer.score(neighbors.nodes()[i])?;
            if neighbor_similarity >= score {
                return Ok(false);
            }
        }
        Ok(true)
    }
    fn get_random_graph_level(ml: f64, random: &mut ChaCha20Rng) -> usize {
        loop {
            let rand_double: f64 = random.next_u64() as f64;
            if rand_double > 0.0 {
                return (-rand_double.ln() * ml).floor() as usize;
            }
        }
    }
    pub(crate) fn finish(&mut self) -> Result<()> {
        self.connect_all_components()?;
        self.frozen = true;
        Ok(())
    }
    fn connect_all_components(&mut self) -> Result<()> {
        let start = Instant::now();

        for level in 0..self.hnsw.num_levels()? {
            match self.connect_components_with_level(level) {
                Ok(false) => {
                    if self.info_stream.enabled(HNSW_COMPONENT) {
                        self.info_stream.message(
                            HNSW_COMPONENT,
                            &format!("connectComponents failed on level {level}"),
                        );
                    }
                },
                Err(e) => return Err(e),
                _ => {},
            }
        }

        if self.info_stream.enabled(HNSW_COMPONENT) {
            let elapsed = start.elapsed().as_millis();
            self.info_stream
                .message(HNSW_COMPONENT, &format!("connectComponents {elapsed} ms"));
        }

        Ok(())
    }

    fn connect_components_with_level(&mut self, level: usize) -> Result<bool> {
        debug_assert!(self.hnsw.size() <= i32::MAX as usize);
        let mut not_fully_connected = Some(FixedBitSet::new(self.hnsw.size()));
        let mut max_conn = self.m;
        if level == 0 {
            max_conn *= 2;
        }

        let components =
            HnswUtil::components(&mut self.hnsw, level, &mut not_fully_connected, max_conn)?;

        if self.info_stream.enabled(HNSW_COMPONENT) {
            self.info_stream.message(
                HNSW_COMPONENT,
                &format!("connect {} components on level={}", components.len(), level),
            );
        }

        let mut result = true;

        if components.len() > 1 {
            let (c0_index, c0) = components
                .iter()
                .enumerate()
                .max_by_key(|(_, c)| c.size)
                .unwrap();

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

                if self.info_stream.enabled(HNSW_COMPONENT) {
                    self.info_stream.message(
                        HNSW_COMPONENT,
                        &format!("connect component {c:?} to {c0:?}"),
                    );
                }

                beam.clear();
                eps[0] = c0.start;
                let mut scorer = self.scorer_supplier.scorer(c.start)?;
                // find the closest node in the largest component to the lowest-numbered node in
                // this component that has room to make a connection
                self.graph_searcher.search_level_with_collector(
                    &mut beam,
                    &mut scorer,
                    level,
                    &eps,
                    &mut self.hnsw,
                    &mut not_fully_connected,
                )?;

                let mut linked = false;

                while beam.size() > 0 {
                    let c0node = beam.pop_node()?;
                    if c0node == c.start || !not_fully_connected.as_ref().unwrap().get(c0node)? {
                        continue;
                    }

                    let score = beam.minimum_score();
                    debug_assert!(not_fully_connected.as_ref().unwrap().get(c0node)?);
                    // link the nodes
                    self.link(
                        level,
                        c0node,
                        c.start,
                        score,
                        not_fully_connected.as_mut().unwrap(),
                    )?;

                    linked = true;

                    if self.info_stream.enabled(HNSW_COMPONENT) {
                        self.info_stream.message(
                            HNSW_COMPONENT,
                            &format!("connected ok {} -> {}", c0node, c.start),
                        );
                    }
                }

                if !linked {
                    if self.info_stream.enabled(HNSW_COMPONENT) {
                        self.info_stream
                            .message(HNSW_COMPONENT, "not connected; no free nodes found");
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
        &mut self,
        level: usize,
        n0: usize,
        n1: usize,
        score: f32,
        not_fully_connected: &mut FixedBitSet,
    ) -> Result<()> {
        let HnswGraphEnums::OnHeap(hnsw) = &mut self.hnsw;
        let nbr0 = hnsw.get_neighbors(level, n0);
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
            not_fully_connected.clear_with_index(n0);
        }

        let nbr1 = hnsw.get_neighbors(level, n1);
        if nbr1.size() < max_conn {
            nbr1.add_out_of_order(n0, score)?;
            if nbr1.size() == max_conn {
                not_fully_connected.clear_with_index(n1);
            }
        }

        Ok(())
    }
}
impl<S, B, H> HnswBuilder for HnswGraphBuilder<S, B, H>
where
    S: RandomVectorScorerSupplier,
    B: BitSet,
    H: HnswGraphSearcherBase,
{
    fn build(&mut self, max_ord: usize) -> Result<&mut OnHeapHnswGraph> {
        if self.frozen {
            return Err(LuceneError::illegal_state(
                "This HnswGraphBuilder is frozen and cannot be updated",
            ));
        }

        if self.info_stream.enabled(HNSW_COMPONENT) {
            self.info_stream.message(
                HNSW_COMPONENT,
                &format!("build graph from {max_ord} vectors"),
            );
        }

        self.add_all_vectors(max_ord)?;
        self.get_completed_graph()
    }

    fn add_graph_node(&mut self, node: usize) -> Result<()> {
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
        if self.frozen {
            return Err(LuceneError::illegal_state(
                "Graph builder is already frozen",
            ));
        }

        let mut scorer = self.scorer_supplier.scorer(node)?;

        let node_level = Self::get_random_graph_level(self.ml, &mut self.random);

        {
            let HnswGraphEnums::OnHeap(hnsw) = &mut self.hnsw;
            // first add nodes to all levels
            for level in (0..=node_level).rev() {
                hnsw.add_node(level, node)?;
            }
            // then promote itself as entry node if entry node is not set
            if hnsw.try_set_new_entry_node(node, node_level) {
                return Ok(());
            }
        }

        // if the entry node is already set, then we have to do all connections first
        // before we can promote ourselves as entry node
        let mut lowest_unset_level = 0;

        let mut cur_max_level;
        loop {
            let mut eps = {
                let HnswGraphEnums::OnHeap(hnsw) = &mut self.hnsw;
                cur_max_level = hnsw.num_levels()? - 1;
                // NOTE: the entry node and max level may not be paired, but because we get the
                // level first we ensure that the entry node we get later will
                // always exist on the curMaxLevel
                match hnsw.entry_node()? {
                    Some(v) => {
                        vec![v]
                    },
                    None => {
                        return Err(LuceneError::illegal_state(
                            "Entry node is not set when trying to add connections",
                        ));
                    },
                }
            };
            // we first do the search from top to bottom
            // for levels > nodeLevel search with topk = 1
            let candidates = &mut self.entry_candidates;
            for level in (node_level + 1..=cur_max_level).rev() {
                candidates.clear();
                self.graph_searcher.search_level_with_collector(
                    candidates,
                    &mut scorer,
                    level,
                    &eps,
                    &mut self.hnsw,
                    &mut None::<B>,
                )?;
                eps[0] = candidates.pop_node()?;
            }

            // for levels <= nodeLevel search with topk = beamWidth, and add connections
            let candidates = &mut self.beam_candidates;
            let top = std::cmp::min(node_level, cur_max_level);
            let mut scratch_per_level =
                vec![NeighborArray::default(); top - lowest_unset_level + 1];

            for i in (0..scratch_per_level.len()).rev() {
                let level = i + lowest_unset_level;
                candidates.clear();
                self.graph_searcher.search_level_with_collector(
                    candidates,
                    &mut scorer,
                    level,
                    &eps,
                    &mut self.hnsw,
                    &mut None::<B>,
                )?;
                eps = candidates.pop_until_nearest_k_nodes()?;
                let mut scratch =
                    NeighborArray::new(std::cmp::max(candidates.k(), self.m + 1), false);
                Self::pop_to_scratch(candidates, &mut scratch)?;
                scratch_per_level[i] = scratch;
            }

            // then do connections from bottom up
            for (i, scratch) in scratch_per_level.iter_mut().enumerate() {
                self.add_diverse_neighbors(i + lowest_unset_level, node, scratch)?;
            }

            lowest_unset_level = scratch_per_level.len() + 1;
            debug_assert!(lowest_unset_level == (std::cmp::min(cur_max_level, node_level) + 1));
            if lowest_unset_level > node_level {
                return Ok(());
            }

            debug_assert!(lowest_unset_level == (cur_max_level + 1) && node_level > cur_max_level);
            let HnswGraphEnums::OnHeap(hnsw) = &mut self.hnsw;
            if hnsw.try_promote_new_entry_node(node, node_level, cur_max_level) {
                return Ok(());
            }

            if hnsw.num_levels()? == cur_max_level + 1 {
                // This should never happen if all the calculations are correct
                return Err(LuceneError::illegal_state(format!(
                    "Unable to promote node {node} at level {node_level} as entry. Graph level {cur_max_level} has not changed."
                )));
            }
        }
    }

    fn set_info_stream(&mut self, info_stream: InfoStreamMT) {
        self.info_stream = info_stream;
    }

    fn get_graph(&mut self) -> &mut OnHeapHnswGraph {
        match &mut self.hnsw {
            HnswGraphEnums::OnHeap(graph) => graph,
        }
    }

    fn get_completed_graph(&mut self) -> Result<&mut OnHeapHnswGraph> {
        if !self.frozen {
            self.finish()?;
        }
        Ok(self.get_graph())
    }
}

/// A restricted, specialized [`KnnCollector`] that can be used when building a
/// graph.
///
/// This collector does **not** support [`TopDocs`].
pub struct GraphBuilderKnnCollector {
    queue: NeighborQueue,
    k: usize,
    visited_count: usize,
}
impl GraphBuilderKnnCollector {
    pub fn new(k: usize) -> Result<Self> {
        Ok(Self {
            queue: NeighborQueue::new(k, false)?,
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
        while self.size() > self.k {
            self.queue.pop()?;
        }
        Ok(self.queue.nodes())
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

    fn collect(&mut self, doc_id: usize, similarity: f32) -> bool {
        self.queue.insert_with_overflow(doc_id, similarity)
    }

    fn min_competitive_similarity(&self) -> f32 {
        if self.queue.size() >= self.k {
            self.queue.top_score()
        } else {
            f32::NEG_INFINITY
        }
    }

    type Item = DummyScoreDocLike;

    fn top_docs(&mut self) -> Result<TopDocs<Self::Item>> {
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

pub fn create<S>(
    scorer_supplier: S,
    m: usize,
    beam_width: usize,
    random: ChaCha20Rng,
) -> Result<HnswGraphBuilder<S, FixedBitSet, HnswGraphSearcherBaseDefault>>
where
    S: RandomVectorScorerSupplier,
{
    HnswGraphBuilder::from_graph_size(scorer_supplier, m, beam_width, random, -1)
}

/// Equivalent to `HnswGraphBuilder::create(scorerSupplier, M, beamWidth,
/// seed, graphSize)`
pub fn create_with_graph_size<S>(
    scorer_supplier: S,
    m: usize,
    beam_width: usize,
    random: ChaCha20Rng,
    graph_size: i32,
) -> Result<HnswGraphBuilder<S, FixedBitSet, HnswGraphSearcherBaseDefault>>
where
    S: RandomVectorScorerSupplier,
{
    HnswGraphBuilder::from_graph_size(scorer_supplier, m, beam_width, random, graph_size)
}
