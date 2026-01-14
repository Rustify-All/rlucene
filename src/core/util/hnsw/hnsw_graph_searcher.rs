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
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::search::knn_collector::KnnCollector;
use crate::core::util::bit_set::BitSet;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::hnsw::hnsw_graph::{HnswGraph, HnswGraphEnums};
use crate::core::util::hnsw::hnsw_graph_builder::GraphBuilderKnnCollector;
use crate::core::util::hnsw::neighbor_queue::NeighborQueue;
use crate::core::util::hnsw::random_vector_scorer::RandomVectorScorer;
/// Searches an HNSW graph to find nearest neighbors to a query vector.
///
/// For more background on the search algorithm, see [`HnswGraph`].
pub struct HnswGraphSearcher<B, H>
where
    H: HnswGraphSearcherBase,
    B: BitSet,
{
    /// Scratch data structures that are used in each `search_level` call.
    /// These can be expensive to allocate, so they're cleared and reused across
    /// calls.
    candidates: NeighborQueue,
    visited: B,
    sub: H,
}

impl<B, H> HnswGraphSearcher<B, H>
where
    H: HnswGraphSearcherBase,
    B: BitSet,
{
    /// Creates a new graph searcher.
    ///
    /// # Arguments
    ///
    /// * `candidates` - max heap that will track the candidate nodes to explore
    /// * `visited` - bit set that will track nodes that have already been
    ///   visited
    pub fn new(candidates: NeighborQueue, visited: B, sub: H) -> Self {
        Self {
            candidates,
            visited,
            sub,
        }
    }
    /// Searches for the nearest neighbors of a query vector in a given level.
    ///
    /// If the search stops early because it reaches the visited nodes limit,
    /// then the results will be marked incomplete through
    /// [`NeighborQueue::incomplete`].
    ///
    /// # Arguments
    ///
    /// * `scorer` - the scorer to compare the query with the nodes
    /// * `top_k` - the number of nearest to query results to return
    /// * `level` - level to search
    /// * `eps` - the entry points for search at this level expressed as level
    ///   0th ordinals
    /// * `graph` - the graph values
    ///
    /// # Returns
    ///
    /// A set of collected vectors holding the nearest neighbors found
    pub fn search_level<S>(
        &mut self,
        scorer: &mut S,
        top_k: usize,
        level: usize,
        eps: &[usize],
        graph: &mut HnswGraphEnums,
    ) -> Result<GraphBuilderKnnCollector>
    where
        S: RandomVectorScorer,
    {
        let mut results = GraphBuilderKnnCollector::new(top_k)?;
        self.search_level_with_collector(
            &mut results,
            scorer,
            level,
            eps,
            graph,
            &mut None::<FixedBitSet>,
        )?;
        Ok(results)
    }
    /// Function to find the best entry point from which to search the zeroth
    /// graph layer.
    ///
    /// # Arguments
    ///
    /// * `scorer` - the scorer to compare the query with the nodes
    /// * `graph` - the HNSW graph
    /// * `collector` - the knn result collector
    ///
    /// # Returns
    ///
    /// The best entry point. `-1` indicates the graph entry node is not set, or
    /// the visitation limit was exceeded.
    fn find_best_entry_point<S>(
        &mut self,
        scorer: &mut S,
        graph: &mut HnswGraphEnums,
        collector: &mut impl KnnCollector,
    ) -> Result<Option<usize>>
    where
        S: RandomVectorScorer,
    {
        let current_ep = graph.entry_node()?;
        if current_ep.is_none() || graph.num_levels()? == 1 {
            return Ok(current_ep);
        }
        let mut current_ep = *current_ep.as_ref().unwrap();
        let size = get_graph_size(graph);
        self.prepare_scratch_state(size);

        let mut current_score = scorer.score(current_ep)?;
        collector.inc_visited_count(1);

        for level in (1..graph.num_levels()?).rev() {
            let mut found_better = true;
            self.visited.set(current_ep);
            // Keep searching the given level until we stop finding a better candidate entry
            // point
            while found_better {
                found_better = false;
                self.sub.graph_seek(graph, level, current_ep)?;
                let mut friend_ord: usize;
                while {
                    friend_ord = self.sub.graph_next_neighbor(graph)?;
                    friend_ord != NO_MORE_DOCS as usize
                } {
                    debug_assert!(friend_ord < size, "friendOrd={friend_ord} >= size={size}");

                    if self.visited.get_and_set(friend_ord) {
                        continue;
                    }

                    if collector.early_terminated() {
                        return Ok(None);
                    }

                    let friend_score = scorer.score(friend_ord)?;
                    collector.inc_visited_count(1);

                    if friend_score > current_score {
                        current_score = friend_score;
                        current_ep = friend_ord;
                        found_better = true;
                    }
                }
            }
        }

        Ok(if collector.early_terminated() {
            None
        } else {
            Some(current_ep)
        })
    }
    /// Add the closest neighbors found to a priority queue (heap).
    ///
    /// These are returned in **REVERSE** proximity order —
    /// the most distant neighbor of the topK found (i.e., the one with the
    /// lowest score/comparison value) will be at the top of the heap, while
    /// the closest neighbor will be the last to be popped.
    pub(crate) fn search_level_with_collector<S>(
        &mut self,
        results: &mut impl KnnCollector,
        scorer: &mut S,
        level: usize,
        eps: &[usize],
        graph: &mut HnswGraphEnums,
        accept_ords: &mut Option<impl Bits>,
    ) -> Result<()>
    where
        S: RandomVectorScorer,
    {
        let size = get_graph_size(graph);
        self.prepare_scratch_state(size);

        for &ep in eps {
            if !self.visited.get_and_set(ep) {
                if results.early_terminated() {
                    break;
                }
                let score = scorer.score(ep)?;
                results.inc_visited_count(1);
                self.candidates.add(ep, score);
                if accept_ords.is_none() || accept_ords.as_ref().unwrap().get(ep)? {
                    results.collect(ep, score);
                }
            }
        }
        // A bound that holds the minimum similarity to the query vector that a
        // candidate vector must have to be considered.
        let mut min_accepted_similarity = results.min_competitive_similarity();
        while self.candidates.size() > 0 && !results.early_terminated() {
            let top_candidate_similarity = self.candidates.top_score();
            if top_candidate_similarity < min_accepted_similarity {
                break;
            }

            let top_node = self.candidates.pop()?;
            self.sub.graph_seek(graph, level, top_node)?;
            let mut friend_ord;
            while {
                friend_ord = self.sub.graph_next_neighbor(graph)?;
                friend_ord != NO_MORE_DOCS as usize
            } {
                debug_assert!(friend_ord < size, "friendOrd={friend_ord} >= size={size}");

                if self.visited.get_and_set(friend_ord) {
                    continue;
                }

                if results.early_terminated() {
                    break;
                }

                let friend_similarity = scorer.score(friend_ord)?;
                results.inc_visited_count(1);

                if friend_similarity > min_accepted_similarity {
                    self.candidates.add(friend_ord, friend_similarity);
                    if (accept_ords.is_none() || accept_ords.as_ref().unwrap().get(friend_ord)?)
                        && results.collect(friend_ord, friend_similarity)
                    {
                        min_accepted_similarity = results.min_competitive_similarity();
                    }
                }
            }
        }

        Ok(())
    }
    fn prepare_scratch_state(&mut self, capacity: usize) {
        self.candidates.clear();
        if self.visited.length() < capacity {
            debug_assert!(capacity <= i32::MAX as usize);
            self.visited.ensure_capacity(capacity);
        }
        self.visited.clear();
    }
}

pub trait HnswGraphSearcherBase {
    /// Seek a specific node in the given graph.
    ///
    /// The default implementation will just call [`HnswGraph::seek`].
    ///
    /// # Errors
    ///
    /// Returns an error if seeking the graph fails.
    fn graph_seek(
        &mut self,
        graph: &mut HnswGraphEnums,
        level: usize,
        target_node: usize,
    ) -> Result<()> {
        graph.seek(level, target_node)
    }
    /// Get the next neighbor from the graph.
    ///
    /// You must call [`Self::graph_seek`] before calling this method.
    /// The default implementation will just call [`HnswGraph::next_neighbor`].
    ///
    /// # Returns
    ///
    /// See [`HnswGraph::next_neighbor`] for details.
    ///
    /// # Errors
    ///
    /// Returns an error if advancing to the next neighbor fails.
    fn graph_next_neighbor(&mut self, graph: &mut HnswGraphEnums) -> Result<usize> {
        graph.next_neighbor()
    }
}
#[derive(Default)]
pub(crate) struct HnswGraphSearcherBaseDefault;
impl HnswGraphSearcherBase for HnswGraphSearcherBaseDefault {}

/// This struct allows [`OnHeapHnswGraph`] to be searched in a thread-safe
/// manner by avoiding the unsafe methods (`seek` and `next_neighbor`, which
/// maintain state in the graph object), and instead maintaining the state in
/// the searcher instance.
///
/// **Note**: The struct itself is **NOT** thread-safe, but since each search
/// creates a new `Searcher`, the search methods using this struct are
/// thread-safe.
#[derive(Default)]
pub(crate) struct OnHeapHnswGraphSearcher {
    cur_level: usize,
    cur_node: usize,
    upto: i32,
}
impl HnswGraphSearcherBase for OnHeapHnswGraphSearcher {
    fn graph_seek(
        &mut self,
        _graph: &mut HnswGraphEnums,
        level: usize,
        target_node: usize,
    ) -> Result<()> {
        self.cur_level = level;
        self.cur_node = target_node;
        self.upto = -1;
        Ok(())
    }

    fn graph_next_neighbor(&mut self, graph: &mut HnswGraphEnums) -> Result<usize> {
        match graph {
            HnswGraphEnums::OnHeap(graph) => {
                let neighbors = graph.get_neighbors(self.cur_level, self.cur_node);
                self.upto += 1;
                if (self.upto as usize) < neighbors.size() {
                    Ok(neighbors.nodes()[self.upto as usize])
                } else {
                    Ok(NO_MORE_DOCS as usize)
                }
            },
        }
    }
}
use crate::core::search::top_knn_collector::TopKnnCollector;
use crate::core::util::sparse_fixed_bit_set::SparseFixedBitSet;
/// Searches HNSW graph for the nearest neighbors of a query vector.
///
/// # Arguments
///
/// * `scorer` - the scorer to compare the query with the nodes
/// * `knn_collector` - a collector of top knn results to be returned
/// * `graph` - the graph values. May represent the entire graph, or a level
///   in a hierarchical graph
/// * `accept_ords` - a [`Bits`] instance that represents the allowed
///   document ordinals to match, or `None` if all are allowed to match
pub fn search<S>(
    scorer: &mut S,
    knn_collector: &mut impl KnnCollector,
    graph: &mut HnswGraphEnums,
    accept_ords: &mut Option<impl Bits>,
) -> Result<()>
where
    S: RandomVectorScorer,
{
    let bitset = SparseFixedBitSet::new(get_graph_size(graph))?;
    let top_k = knn_collector.k();
    let neighbor_queue = NeighborQueue::new(top_k, true)?;
    let mut graph_searcher =
        HnswGraphSearcher::new(neighbor_queue, bitset, HnswGraphSearcherBaseDefault);
    search_with_searcher(
        scorer,
        knn_collector,
        graph,
        &mut graph_searcher,
        accept_ords,
    )
}
/// Search [`OnHeapHnswGraph`], this method is thread-safe.
///
/// # Arguments
///
/// * `scorer` - the scorer to compare the query with the nodes
/// * `top_k` - the number of nodes to be returned
/// * `graph` - the graph values. May represent the entire graph, or a level
///   in a hierarchical graph
/// * `accept_ords` - a [`Bits`] instance that represents the allowed
///   document ordinals to match, or `None` if all are allowed to match
/// * `visited_limit` - the maximum number of nodes that the search is
///   allowed to visit
///
/// # Returns
///
/// A set of collected vectors holding the nearest neighbors found
pub fn search_with_top_k<S>(
    scorer: &mut S,
    top_k: usize,
    graph: &mut HnswGraphEnums,
    accept_ords: &mut Option<impl Bits>,
    visited_limit: usize,
) -> Result<TopKnnCollector>
where
    S: RandomVectorScorer,
{
    let mut knn_collector = TopKnnCollector::new(top_k, visited_limit)?;
    let bitset = SparseFixedBitSet::new(get_graph_size(graph))?;
    let neighbor_queue = NeighborQueue::new(top_k, true)?;
    let mut graph_searcher =
        HnswGraphSearcher::new(neighbor_queue, bitset, OnHeapHnswGraphSearcher::default());
    debug_assert!(matches!(graph, HnswGraphEnums::OnHeap(_)));
    search_with_searcher(
        scorer,
        &mut knn_collector,
        graph,
        &mut graph_searcher,
        accept_ords,
    )?;

    Ok(knn_collector)
}
fn search_with_searcher<H, S, B>(
    scorer: &mut S,
    knn_collector: &mut impl KnnCollector,
    graph: &mut HnswGraphEnums,
    graph_searcher: &mut HnswGraphSearcher<B, H>,
    accept_ords: &mut Option<impl Bits>,
) -> Result<()>
where
    H: HnswGraphSearcherBase,
    B: BitSet,
    S: RandomVectorScorer,
{
    if let Some(ep) = graph_searcher.find_best_entry_point(scorer, graph, knn_collector)? {
        graph_searcher.search_level_with_collector(
            knn_collector,
            scorer,
            0,
            &[ep],
            graph,
            accept_ords,
        )?;
    }
    Ok(())
}
pub(crate) fn get_graph_size<G: HnswGraph>(graph: &G) -> usize {
    match graph.max_node_id() {
        Some(v) => v + 1,
        None => 0,
    }
}
