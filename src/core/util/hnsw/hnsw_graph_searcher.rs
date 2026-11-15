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
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::knn_collector::KnnCollector;
use crate::core::util::bit_set::BitSet;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::hnsw::hnsw_concurrent_merge_builder::MergeSearcher;
use crate::core::util::hnsw::hnsw_graph::HnswGraph;
use crate::core::util::hnsw::hnsw_graph_builder::GraphBuilderKnnCollector;
use crate::core::util::hnsw::neighbor_queue::NeighborQueue;
use crate::core::util::hnsw::random_vector_scorer::RandomVectorScorer;
use std::convert::Infallible;
/// Searches an HNSW graph to find nearest neighbors to a query vector.
///
/// For more background on the search algorithm, see [`HnswGraph`].
pub struct HnswGraphSearcher<B, const DEFAULT_HOOK: bool = true> {
  /// Scratch data structures that are used in each `search_level` call.
  /// These can be expensive to allocate, so they're cleared and reused across
  /// calls.
  candidates: NeighborQueue<true>,
  visited: B,
  hook: HnswGraphSearcherHook,
}

#[derive(Default)]
pub(crate) enum HnswGraphSearcherHook {
  #[default]
  Default,
  OnHeap(OnHeapHnswGraphSearcher),
  Merge(MergeSearcher),
}

pub(crate) struct HnswGraphSearcherDefaults;

impl<B> HnswGraphSearcher<B>
where
  B: BitSet,
{
  /// Creates a new graph searcher.
  ///
  /// # Arguments
  ///
  /// * `candidates` - max heap that will track the candidate nodes to explore
  /// * `visited` - bit set that will track nodes that have already been
  ///   visited
  pub fn new(candidates: NeighborQueue<true>, visited: B) -> Self {
    Self {
      candidates,
      visited,
      hook: HnswGraphSearcherHook::Default,
    }
  }
}

impl<B> HnswGraphSearcher<B, false>
where
  B: BitSet,
{
  pub(crate) fn with_hook(
    candidates: NeighborQueue<true>,
    visited: B,
    hook: HnswGraphSearcherHook,
  ) -> Self {
    Self {
      candidates,
      visited,
      hook,
    }
  }
}

impl<B, const DEFAULT_HOOK: bool> HnswGraphSearcher<B, DEFAULT_HOOK>
where
  B: BitSet,
{
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
  pub fn search_level<S, T>(
    &mut self,
    scorer: &S,
    top_k: usize,
    level: usize,
    eps: &[usize],
    graph: &mut T,
  ) -> Result<GraphBuilderKnnCollector>
  where
    S: RandomVectorScorer,
    T: HnswGraph,
  {
    let mut results = GraphBuilderKnnCollector::new(top_k)?;
    self.search_level_with_collector(
      &mut results,
      scorer,
      level,
      eps,
      graph,
      &None::<Infallible>,
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
  fn find_best_entry_point<S, T, V>(
    &mut self,
    scorer: &S,
    graph: &mut T,
    collector: &mut V,
  ) -> Result<Option<usize>>
  where
    S: RandomVectorScorer,
    T: HnswGraph,
    V: KnnCollector,
  {
    let Some(mut current_ep) = graph.entry_node()? else {
      return Ok(None);
    };
    if graph.num_levels()? == 1 {
      return Ok(Some(current_ep));
    }
    let size = get_graph_size(graph);
    self.prepare_scratch_state(size)?;

    let mut current_score = scorer.score(current_ep)?;
    collector.inc_visited_count(1);

    for level in (1..graph.num_levels()?).rev() {
      let mut found_better = true;
      self.visited.set(current_ep)?;
      // Keep searching the given level until we stop finding a better candidate entry
      // point
      while found_better {
        found_better = false;
        self.graph_seek(graph, level, current_ep)?;
        let mut friend_ord: usize;
        while {
          friend_ord = self.graph_next_neighbor(graph)?;
          friend_ord != NO_MORE_DOCS as usize
        } {
          debug_assert!(friend_ord < size, "friendOrd={friend_ord} >= size={size}");

          if self.visited.get_and_set(friend_ord)? {
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
  pub(crate) fn search_level_with_collector<S, V, T, T2>(
    &mut self,
    results: &mut V,
    scorer: &S,
    level: usize,
    eps: &[usize],
    mut graph: T,
    accept_ords: &Option<T2>,
  ) -> Result<()>
  where
    S: RandomVectorScorer,
    V: KnnCollector,
    T: HnswGraph,
    T2: Bits,
  {
    let size = get_graph_size(&graph);
    self.prepare_scratch_state(size)?;

    for &ep in eps {
      if !self.visited.get_and_set(ep)? {
        if results.early_terminated() {
          break;
        }
        let score = scorer.score(ep)?;
        results.inc_visited_count(1);
        self.candidates.add(ep, score)?;
        if match accept_ords.as_ref() {
          None => true,
          Some(bits) => bits.get(ep)?,
        } {
          results.collect(ep, score)?;
        }
      }
    }
    // A bound that holds the minimum similarity to the query vector that a
    // candidate vector must have to be considered.
    let mut min_accepted_similarity = results.min_competitive_similarity()?;
    while self.candidates.size() > 0 && !results.early_terminated() {
      let top_candidate_similarity = self.candidates.top_score();
      if top_candidate_similarity < min_accepted_similarity {
        break;
      }

      let top_node = self.candidates.pop()?;
      self.graph_seek(&mut graph, level, top_node)?;
      let (mut neighbors, mut fallback_graph) = if DEFAULT_HOOK && T::HAS_BORROWED_NEIGHBORS {
        (Some(graph.borrow_neighbors()?), None)
      } else {
        (None, Some(&mut graph))
      };
      let mut friend_ord;
      while {
        friend_ord = if DEFAULT_HOOK && T::HAS_BORROWED_NEIGHBORS {
          let Some((neighbors, upto)) = neighbors.as_mut() else {
            return Err(LuceneError::illegal_state(
              "exclusive neighbors not prepared",
            ));
          };
          let current = **upto;
          **upto += 1;
          if current < neighbors.size() {
            neighbors.nodes()[current] as usize
          } else {
            NO_MORE_DOCS as usize
          }
        } else {
          let Some(graph) = fallback_graph.as_mut() else {
            return Err(LuceneError::illegal_state("graph traversal not retained"));
          };
          self.graph_next_neighbor(graph)?
        };
        friend_ord != NO_MORE_DOCS as usize
      } {
        debug_assert!(friend_ord < size, "friendOrd={friend_ord} >= size={size}");

        if self.visited.get_and_set(friend_ord)? {
          continue;
        }

        if results.early_terminated() {
          break;
        }

        if DEFAULT_HOOK
          && T::HAS_BORROWED_NEIGHBORS
          && let Some((next_neighbors, next_upto)) = neighbors.as_ref()
          && **next_upto < next_neighbors.size()
          && let Some(&next_node) = next_neighbors.nodes().get(**next_upto)
        {
          scorer.prefetch(next_node as usize);
        }

        let friend_similarity = scorer.score(friend_ord)?;
        results.inc_visited_count(1);

        if friend_similarity > min_accepted_similarity {
          self.candidates.add(friend_ord, friend_similarity)?;
          if (match accept_ords.as_ref() {
            None => true,
            Some(bits) => bits.get(friend_ord)?,
          }) && results.collect(friend_ord, friend_similarity)?
          {
            min_accepted_similarity = results.min_competitive_similarity()?;
          }
        }
      }
    }

    Ok(())
  }
  fn graph_seek<T>(&mut self, graph: &mut T, level: usize, target_node: usize) -> Result<()>
  where
    T: HnswGraph,
  {
    if DEFAULT_HOOK {
      HnswGraphSearcherDefaults::graph_seek(graph, level, target_node)
    } else {
      self.hook.graph_seek(graph, level, target_node)
    }
  }
  fn graph_next_neighbor<T>(&mut self, graph: &mut T) -> Result<usize>
  where
    T: HnswGraph,
  {
    if DEFAULT_HOOK {
      HnswGraphSearcherDefaults::graph_next_neighbor(graph)
    } else {
      self.hook.graph_next_neighbor(graph)
    }
  }
  fn prepare_scratch_state(&mut self, capacity: usize) -> Result<()> {
    self.candidates.clear();
    if self.visited.length() < capacity {
      debug_assert!(capacity <= i32::MAX as usize);
      self.visited.ensure_capacity(capacity)?;
    }
    self.visited.clear()?;
    Ok(())
  }
}

pub(crate) trait HnswGraphSearcherBase {
  /// Seek a specific node in the given graph.
  ///
  /// The default implementation will just call [`HnswGraph::seek`].
  ///
  /// # Errors
  ///
  /// Returns an error if seeking the graph fails.
  fn graph_seek<T>(&mut self, graph: &mut T, level: usize, target_node: usize) -> Result<()>
  where
    T: HnswGraph,
  {
    HnswGraphSearcherDefaults::graph_seek(graph, level, target_node)
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
  fn graph_next_neighbor<T>(&mut self, graph: &mut T) -> Result<usize>
  where
    T: HnswGraph,
  {
    HnswGraphSearcherDefaults::graph_next_neighbor(graph)
  }
}

impl HnswGraphSearcherDefaults {
  fn graph_seek<T>(graph: &mut T, level: usize, target_node: usize) -> Result<()>
  where
    T: HnswGraph,
  {
    graph.seek(level, target_node)
  }
  fn graph_next_neighbor<T>(graph: &mut T) -> Result<usize>
  where
    T: HnswGraph,
  {
    graph.next_neighbor()
  }
}

impl HnswGraphSearcherBase for HnswGraphSearcherHook {
  fn graph_seek<T>(&mut self, graph: &mut T, level: usize, target_node: usize) -> Result<()>
  where
    T: HnswGraph,
  {
    match self {
      Self::Default => HnswGraphSearcherDefaults::graph_seek(graph, level, target_node),
      Self::OnHeap(hook) => hook.graph_seek(graph, level, target_node),
      Self::Merge(hook) => hook.graph_seek(graph, level, target_node),
    }
  }

  fn graph_next_neighbor<T>(&mut self, graph: &mut T) -> Result<usize>
  where
    T: HnswGraph,
  {
    match self {
      Self::Default => HnswGraphSearcherDefaults::graph_next_neighbor(graph),
      Self::OnHeap(hook) => hook.graph_next_neighbor(graph),
      Self::Merge(hook) => hook.graph_next_neighbor(graph),
    }
  }
}

/// This struct allows [`OnHeapHnswGraph`] to be searched in a thread-safe
/// manner by avoiding the unsafe methods (`seek` and `next_neighbor`, which
/// maintain state in the graph object), and instead maintaining the state in
/// the searcher instance.
///
/// **Note**: The struct itself is **NOT** thread-safe, but since each search
/// creates a new `Searcher`, the search methods using this struct are
/// thread-safe.
pub(crate) struct OnHeapHnswGraphSearcher {
  cur_level: usize,
  cur_node: usize,
  upto: Option<usize>,
}
impl Default for OnHeapHnswGraphSearcher {
  fn default() -> Self {
    Self {
      cur_level: 0,
      cur_node: 0,
      upto: Some(0),
    }
  }
}
impl OnHeapHnswGraphSearcher {
  fn graph_seek<T>(&mut self, _graph: &T, level: usize, target_node: usize) -> Result<()>
  where
    T: HnswGraph,
  {
    self.cur_level = level;
    self.cur_node = target_node;
    self.upto = None;
    Ok(())
  }

  fn graph_next_neighbor<T>(&mut self, graph: &T) -> Result<usize>
  where
    T: HnswGraph,
  {
    graph.with_neighbors(self.cur_level, self.cur_node, |neighbors| {
      let upto = self.upto.map_or(0, |upto| upto + 1);
      self.upto = Some(upto);
      if upto < neighbors.size() {
        Ok(neighbors.nodes()[upto] as usize)
      } else {
        Ok(NO_MORE_DOCS as usize)
      }
    })
  }
}
use crate::core::search::top_knn_collector::TopKnnCollector;
use crate::core::util::hnsw::on_heap_hnsw_graph::OnHeapHnswGraph;
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
pub fn search<S, V, T, T2>(
  scorer: &S,
  knn_collector: &mut V,
  graph: &mut T,
  accept_ords: Option<&T2>,
) -> Result<()>
where
  S: RandomVectorScorer,
  V: KnnCollector,
  T: HnswGraph,
  T2: Bits,
{
  let bitset = SparseFixedBitSet::new(get_graph_size(graph))?;
  let top_k = knn_collector.k();
  let neighbor_queue = NeighborQueue::<true>::new(top_k)?;
  let mut graph_searcher = HnswGraphSearcher::new(neighbor_queue, bitset);
  search_with_searcher(
    scorer,
    knn_collector,
    graph,
    &mut graph_searcher,
    accept_ords,
  )
}
/// Searches an [`OnHeapHnswGraph`]; this function is thread-safe.
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
pub fn search_with_top_k<S, T>(
  scorer: &S,
  top_k: usize,
  graph: &OnHeapHnswGraph,
  accept_ords: Option<&T>,
  visited_limit: usize,
) -> Result<TopKnnCollector>
where
  S: RandomVectorScorer,
  T: Bits,
{
  let mut knn_collector = TopKnnCollector::new(top_k, visited_limit)?;
  let bitset = SparseFixedBitSet::new(get_graph_size(graph))?;
  let neighbor_queue = NeighborQueue::<true>::new(top_k)?;
  let mut graph_searcher = HnswGraphSearcher::with_hook(
    neighbor_queue,
    bitset,
    HnswGraphSearcherHook::OnHeap(OnHeapHnswGraphSearcher::default()),
  );
  search_with_searcher(
    scorer,
    &mut knn_collector,
    graph,
    &mut graph_searcher,
    accept_ords,
  )?;

  Ok(knn_collector)
}
struct MappedFloatScorer<'a, S> {
  parent: &'a S,
  vectors: &'a [f32],
  dimension: usize,
}

impl<S: RandomVectorScorer> RandomVectorScorer for MappedFloatScorer<'_, S> {
  #[inline]
  fn score(&self, node: usize) -> Result<f32> {
    if let Some(start) = node.checked_mul(self.dimension)
      && let Some(end) = start.checked_add(self.dimension)
      && let Some(vector) = self.vectors.get(start..end)
    {
      self.parent.score_mapped_float(node, vector)
    } else {
      self.parent.score(node)
    }
  }

  fn max_ord(&self) -> usize {
    self.parent.max_ord()
  }

  fn ord_to_doc(&self, ord: usize) -> Result<usize> {
    self.parent.ord_to_doc(ord)
  }

  type Bits<'a, B>
    = S::Bits<'a, B>
  where
    Self: 'a,
    B: Bits;

  fn get_accept_ords<'a, B: Bits>(
    &'a self,
    accept_docs: Option<B>,
  ) -> Result<Option<Self::Bits<'a, B>>> {
    self.parent.get_accept_ords(accept_docs)
  }
}

pub(crate) fn search_with_searcher<S, B, V, T, T2, const DEFAULT_HOOK: bool>(
  scorer: &S,
  knn_collector: &mut V,
  graph: T,
  graph_searcher: &mut HnswGraphSearcher<B, DEFAULT_HOOK>,
  accept_ords: Option<&T2>,
) -> Result<()>
where
  B: BitSet,
  S: RandomVectorScorer,
  V: KnnCollector,
  T: HnswGraph,
  T2: Bits,
{
  if let Some(vectors) = scorer.mapped_float_vectors()
    && let Some(values) = vectors.as_slice()
  {
    let mapped = MappedFloatScorer {
      parent: scorer,
      vectors: values,
      dimension: vectors.dimension(),
    };
    search_with_searcher_impl(&mapped, knn_collector, graph, graph_searcher, accept_ords)
  } else {
    search_with_searcher_impl(scorer, knn_collector, graph, graph_searcher, accept_ords)
  }
}

fn search_with_searcher_impl<S, B, V, T, T2, const DEFAULT_HOOK: bool>(
  scorer: &S,
  knn_collector: &mut V,
  mut graph: T,
  graph_searcher: &mut HnswGraphSearcher<B, DEFAULT_HOOK>,
  accept_ords: Option<&T2>,
) -> Result<()>
where
  B: BitSet,
  S: RandomVectorScorer,
  V: KnnCollector,
  T: HnswGraph,
  T2: Bits,
{
  if let Some(ep) = graph_searcher.find_best_entry_point(scorer, &mut graph, knn_collector)? {
    graph_searcher.search_level_with_collector(
      knn_collector,
      scorer,
      0,
      &[ep],
      graph,
      &accept_ords,
    )?;
  }
  Ok(())
}
pub(crate) fn get_graph_size<G>(graph: &G) -> usize
where
  G: HnswGraph,
{
  match graph.max_node_id() {
    Some(v) => v + 1,
    None => 0,
  }
}
