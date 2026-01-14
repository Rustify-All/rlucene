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
use std::collections::VecDeque;

use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::util::bit_set::BitSet;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::hnsw::hnsw_graph::{
    ArrayNodesIterator, HnswGraph, NodesIterator, NodesIteratorEnums,
};
/// Utilities for use in tests involving HNSW graphs
pub struct HnswUtil;
impl HnswUtil {
    /*
    For each level, check rooted components from previous level nodes, which are entry
    points with the goal that each node should be reachable from *some* entry point.  For each entry
    point, compute a spanning tree, recording the nodes in a single shared bitset.

    Also record a bitset marking nodes that are not full to be used when reconnecting in order to
    limit the search to include non-full nodes only.
    */
    /// Returns true if every node on every level is reachable from node 0.
    pub(crate) fn is_rooted<G: HnswGraph>(hnsw: &mut G) -> Result<bool> {
        for level in 0..hnsw.num_levels()? {
            let comps = Self::components(hnsw, level, &mut None, 0)?;
            if comps.len() > 1 {
                return Ok(false);
            }
        }
        Ok(true)
    }
    /// Returns the sizes of the distinct graph components on level 0. If the
    /// graph is fully-rooted the list will have one entry. If it is empty, the
    /// returned list will be empty.
    pub(crate) fn component_sizes<G: HnswGraph>(hnsw: &mut G) -> Result<Vec<usize>> {
        Self::component_sizes_on_level(hnsw, 0)
    }
    /// Returns the sizes of the distinct graph components on the given level.
    /// The forest starting at the entry points (nodes in the next highest
    /// level) is considered as a single component. If the entire graph is
    /// rooted in the entry points--that is, every node is reachable from at
    /// least one entry point--the returned list will have a single entry. If
    /// the graph is empty, the returned list will be empty.
    pub(crate) fn component_sizes_on_level<G: HnswGraph>(
        hnsw: &mut G,
        level: usize,
    ) -> Result<Vec<usize>> {
        let comps = Self::components(hnsw, level, &mut None, 0)?;
        Ok(comps.into_iter().map(|c| c.size).collect())
    }

    fn get_total<N: NodesIterator, G: HnswGraph>(
        nodes_iter: N,
        hnsw: &mut G,
        level: usize,
        not_fully_connected: &mut Option<FixedBitSet>,
        connected_nodes: &mut FixedBitSet,
        max_conn: usize,
    ) -> Result<usize> {
        let mut total = 0;
        for entry_point in nodes_iter {
            let component = Self::mark_rooted(
                hnsw,
                level,
                connected_nodes,
                not_fully_connected,
                max_conn,
                entry_point,
            )?;
            total += component.size;
        }
        Ok(total)
    }

    pub(crate) fn components<G: HnswGraph>(
        hnsw: &mut G,
        level: usize,
        not_fully_connected: &mut Option<FixedBitSet>,
        max_conn: usize,
    ) -> Result<Vec<Component>> {
        let mut components = Vec::new();
        debug_assert!(hnsw.size() <= i32::MAX as usize);
        let mut connected_nodes = FixedBitSet::new(hnsw.size());

        assert_eq!(hnsw.size(), hnsw.get_nodes_on_level(0)?.size());

        if level >= hnsw.num_levels()? {
            return Err(LuceneError::illegal_argument(format!(
                "Level {} too large for graph with {} levels",
                level,
                hnsw.num_levels()?
            )));
        }

        let mut total = if level == hnsw.num_levels()? - 1 {
            let v = hnsw.entry_node()?.map(|ep| vec![ep; 1]);
            let iter = NodesIteratorEnums::Array(ArrayNodesIterator::from_nodes(v, 1));
            Self::get_total(
                iter,
                hnsw,
                level,
                not_fully_connected,
                &mut connected_nodes,
                max_conn,
            )?
        } else {
            let iter = hnsw.get_nodes_on_level(level + 1)?;
            Self::get_total(
                iter,
                hnsw,
                level,
                not_fully_connected,
                &mut connected_nodes,
                max_conn,
            )?
        };

        let entry_point = if let Some(nfc) = &not_fully_connected {
            nfc.next_set_bit(0)
        } else {
            connected_nodes.next_set_bit(0)
        };

        components.push(Component {
            start: entry_point,
            size: total,
        });

        if level == 0 {
            let mut next_clear = Self::next_clear_bit(&connected_nodes, 0);
            while next_clear != NO_MORE_DOCS as usize {
                let component = Self::mark_rooted(
                    hnsw,
                    level,
                    &mut connected_nodes,
                    not_fully_connected,
                    max_conn,
                    next_clear,
                )?;
                debug_assert!(component.size > 0);
                components.push(component);
                total += component.size;
                next_clear = Self::next_clear_bit(&connected_nodes, component.start);
            }
        } else {
            let mut nodes = hnsw.get_nodes_on_level(level)?;
            for node in &mut nodes {
                if connected_nodes.get(node)? {
                    continue;
                }
                let component = Self::mark_rooted(
                    hnsw,
                    level,
                    &mut connected_nodes,
                    not_fully_connected,
                    max_conn,
                    node,
                )?;
                debug_assert!(component.size > 0);
                components.push(component);
                total += component.size;
            }
        }

        assert_eq!(
            total,
            hnsw.get_nodes_on_level(level)?.size(),
            "Mismatch total={total} vs node size on level {level}"
        );

        Ok(components)
    }
    /// Count the nodes in a rooted component of the graph and mark them in the
    /// `connected_nodes` bitset. "Rooted" means all nodes reachable from a
    /// specific root node.
    ///
    /// # Parameters
    ///
    /// - `hnsw_graph`: the graph to inspect
    /// - `level`: the specific level of the graph to inspect
    /// - `connected_nodes`: a bitset with the size equal to the number of nodes
    ///   in the graph; this method will mark bits of all nodes reachable from
    ///   the entry point
    /// - `not_fully_connected`: optional bitset (same size) to mark visited
    ///   nodes that have fewer than `max_conn` connections
    /// - `max_conn`: the maximum number of neighbors a node can have (i.e., M)
    /// - `entry_point`: the node ID from which traversal begins
    fn mark_rooted<G: HnswGraph>(
        hnsw_graph: &mut G,
        level: usize,
        connected_nodes: &mut FixedBitSet,
        not_fully_connected: &mut Option<FixedBitSet>,
        max_conn: usize,
        entry_point: usize,
    ) -> Result<Component> {
        // Start at entry point and search all nodes on this level
        let mut stack = VecDeque::new();
        stack.push_back(entry_point);
        let mut count = 0;

        while let Some(node) = stack.pop_back() {
            if connected_nodes.get(node)? {
                continue;
            }
            count += 1;
            connected_nodes.set(node);
            hnsw_graph.seek(level, node)?;

            let mut friend_count = 0;
            let mut friend_ord;
            while {
                friend_ord = hnsw_graph.next_neighbor()?;
                friend_ord != NO_MORE_DOCS as usize
            } {
                friend_count += 1;
                stack.push_back(friend_ord);
            }

            if friend_count < max_conn
                && let Some(nfc) = not_fully_connected
            {
                nfc.set(node);
            }
        }

        Ok(Component {
            start: entry_point,
            size: count,
        })
    }
    fn next_clear_bit(bits: &FixedBitSet, index: usize) -> usize {
        let barray = bits.get_bits();
        debug_assert!(
            index < bits.length(),
            "index={}, num_bits={}",
            index,
            bits.length()
        );

        let mut i = index >> 6;
        let mut word = !(barray[i] >> index);
        let mut next: usize = NO_MORE_DOCS as usize;
        if word != 0 {
            next = index + word.trailing_zeros() as usize;
        } else {
            i += 1;
            while i < barray.len() {
                word = !barray[i];
                if word != 0 {
                    next = (i << 6) + word.trailing_zeros() as usize;
                    break;
                }
                i += 1;
            }
        }

        if next >= bits.length() {
            NO_MORE_DOCS as usize
        } else {
            next
        }
    }
    /// In graph theory, "connected components" are formally defined for
    /// undirected (i.e., bidirectional) graphs. The HNSW graph used here is
    /// directed due to pruning, but it is *mostly* undirected.
    ///
    /// This method evaluates connectivity starting from a single node,
    /// effectively checking whether the graph is a "rooted graph".
    pub fn graph_is_rooted() {
        // TODO: IndexReader not Implemented
    }
}
/// A component (also called "connected component") of an undirected graph is a
/// set of nodes that are connected via neighbor links: every node in the
/// component is reachable from every other node in the same component.  
///
/// See: [Component (graph theory)](https://en.wikipedia.org/wiki/Component_(graph_theory)).
///
/// Such a graph is considered "fully connected" *iff* it has a single
/// component, or it is empty.
///
/// - `start`: the lowest-numbered node in the component
/// - `size`: the number of nodes in the component
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Component {
    pub start: usize,
    pub size: usize,
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use rand::Rng;

    use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
    use crate::core::util::bit_set::BitSet;
    use crate::core::util::bits::Bits;
    use crate::core::util::error::lucene_error::Result;
    use crate::core::util::fixed_bit_set::FixedBitSet;
    use crate::core::util::hnsw::hnsw_graph::{HnswGraph, NodesIterator};
    use crate::core::util::hnsw::hnsw_util::HnswUtil;
    use crate::test::util::lucene_test_case::lucene_test_case_util::{at_least, random};

    #[allow(dead_code)] // for quick search
    struct TestHnswUtil;
    #[test]
    fn test_tree_with_cycle() -> Result<()> {
        let nodes: Vec<Vec<Option<Vec<usize>>>> = vec![vec![
            Some(vec![1, 2]),
            Some(vec![3, 4]),
            Some(vec![5, 6]),
            Some(vec![]),
            Some(vec![]),
            Some(vec![]),
            Some(vec![0]),
        ]];

        let mut graph = MockGraph::new(nodes);

        assert!(HnswUtil::is_rooted(&mut graph)?);
        assert_eq!(HnswUtil::component_sizes(&mut graph)?, vec![7]);

        Ok(())
    }
    #[test]
    fn test_back_linking() -> Result<()> {
        let nodes: Vec<Vec<Option<Vec<usize>>>> = vec![vec![
            Some(vec![1, 2]),
            Some(vec![3, 4]),
            Some(vec![0]),
            Some(vec![1]),
            Some(vec![1]),
            Some(vec![1]),
            Some(vec![1]),
        ]];

        let mut graph = MockGraph::new(nodes);

        assert!(!HnswUtil::is_rooted(&mut graph)?);
        assert_eq!(HnswUtil::component_sizes(&mut graph)?, vec![5, 1, 1]);

        Ok(())
    }
    #[test]
    fn test_chain() -> Result<()> {
        let nodes: Vec<Vec<Option<Vec<usize>>>> = vec![vec![
            Some(vec![1]),
            Some(vec![2]),
            Some(vec![3]),
            Some(vec![0]),
        ]];

        let mut graph = MockGraph::new(nodes);

        assert!(HnswUtil::is_rooted(&mut graph)?);
        assert_eq!(HnswUtil::component_sizes(&mut graph)?, vec![4]);

        Ok(())
    }
    #[test]
    fn test_two_chains() -> Result<()> {
        let nodes: Vec<Vec<Option<Vec<usize>>>> = vec![vec![
            Some(vec![2]),
            Some(vec![3]),
            Some(vec![0]),
            Some(vec![1]),
        ]];

        let mut graph = MockGraph::new(nodes);

        assert!(!HnswUtil::is_rooted(&mut graph)?);
        assert_eq!(HnswUtil::component_sizes(&mut graph)?, vec![2, 2]);

        Ok(())
    }
    #[test]
    fn test_levels() -> Result<()> {
        let nodes: Vec<Vec<Option<Vec<usize>>>> = vec![
            vec![
                Some(vec![1, 2]),
                Some(vec![3]),
                Some(vec![0]),
                Some(vec![0]),
            ],
            vec![Some(vec![2]), None, Some(vec![0]), None],
            vec![Some(vec![]), None, None, None],
        ];

        let mut graph = MockGraph::new(nodes);

        assert!(HnswUtil::is_rooted(&mut graph)?);
        assert_eq!(HnswUtil::component_sizes(&mut graph)?, vec![4]);

        Ok(())
    }
    #[test]
    fn test_levels_not_rooted() -> Result<()> {
        let nodes: Vec<Vec<Option<Vec<usize>>>> = vec![
            vec![Some(vec![1]), Some(vec![0]), Some(vec![0])],
            vec![Some(vec![]), None, None],
        ];
        let mut graph = MockGraph::new(nodes);

        assert!(!HnswUtil::is_rooted(&mut graph)?);
        assert_eq!(HnswUtil::component_sizes(&mut graph)?, vec![2, 1]);

        Ok(())
    }
    #[test]
    fn test_random_graph_rooted_check() -> Result<()> {
        let mut random = random();

        for _ in 0..at_least(&mut random, 10) {
            let num_nodes = random.random_range(1..100);
            let num_levels = (num_nodes as f64).ln().ceil() as usize;
            let mut nodes: Vec<Vec<Option<Vec<usize>>>> = vec![vec![None; num_nodes]; num_levels];

            for level in (0..num_levels).rev() {
                for node in 0..num_nodes {
                    if level > 0 {
                        let higher = level == num_levels - 1;
                        let not_on_above =
                            level < num_levels - 1 && nodes[level + 1][node].is_none();
                        if ((higher && node > 0) || not_on_above)
                            && random.random::<f32>() > (-(level as f32)).exp()
                        {
                            continue;
                        }
                    }

                    let mut num_nbrs = random.random_range(0..num_nodes.div_ceil(8));
                    if level == 0 {
                        num_nbrs *= 2;
                    }

                    nodes[level][node] = Option::from(vec![0; num_nbrs]);
                    for nbr in 0..num_nbrs {
                        loop {
                            let random_nbr = random.random_range(0..num_nodes);
                            if nodes[level][random_nbr].is_some() {
                                nodes[level][node].as_mut().unwrap()[nbr] = random_nbr;
                                break;
                            }
                        }
                    }
                }
            }

            let mut graph = MockGraph::new(nodes.clone());

            let expected = is_rooted(&nodes)?;
            let actual = HnswUtil::is_rooted(&mut graph)?;
            assert_eq!(expected, actual);
        }

        Ok(())
    }

    fn is_rooted(nodes: &[Vec<Option<Vec<usize>>>]) -> Result<bool> {
        for level in (0..nodes.len()).rev() {
            if !is_rooted_with_level(nodes, level)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn is_rooted_with_level(nodes: &[Vec<Option<Vec<usize>>>], level: usize) -> Result<bool> {
        let entry_points: Vec<usize> = if level == nodes.len() - 1 {
            vec![0]
        } else {
            nodes[level + 1]
                .iter()
                .enumerate()
                .filter_map(|(i, node)| node.as_ref().map(|_| i))
                .collect()
        };

        let mut connected = FixedBitSet::new(nodes[level].len());
        let mut count = 0;

        for &entry_point in &entry_points {
            if nodes[level]
                .get(entry_point)
                .and_then(|n| n.as_ref())
                .is_none()
            {
                continue;
            }

            let mut stack = VecDeque::new();
            stack.push_back(entry_point);

            while let Some(node) = stack.pop_back() {
                if connected.get(node)? {
                    continue;
                }
                connected.set(node);
                count += 1;

                if let Some(neighbors) = nodes[level][node].as_ref() {
                    for &nbr in neighbors {
                        stack.push_back(nbr);
                    }
                }
            }
        }

        Ok(count == level_size(&nodes[level]))
    }

    fn level_size(nodes: &[Option<Vec<usize>>]) -> usize {
        let mut count = 0;
        for node in nodes {
            if node.is_some() {
                count += 1;
            }
        }
        count
    }

    pub struct MockGraph {
        nodes: Vec<Vec<Option<Vec<usize>>>>,
        current_level: usize,
        current_node: usize,
        current_neighbor: usize,
    }
    impl MockGraph {
        pub fn new(nodes: Vec<Vec<Option<Vec<usize>>>>) -> Self {
            Self {
                nodes,
                current_level: 0,
                current_node: 0,
                current_neighbor: 0,
            }
        }
    }
    impl HnswGraph for MockGraph {
        fn seek(&mut self, level: usize, target: usize) -> Result<()> {
            assert!(
                level < self.nodes.len(),
                "level {} out of range, max level = {}",
                level,
                self.nodes.len()
            );
            assert!(
                target < self.nodes[level].len(),
                "target {} out of range for level {}, should be less than {}",
                target,
                level,
                self.nodes[level].len()
            );
            assert!(
                self.nodes[level][target].is_some(),
                "target {} not on level {}",
                target,
                level
            );
            self.current_level = level;
            self.current_node = target;
            self.current_neighbor = 0;
            Ok(())
        }

        fn size(&self) -> usize {
            self.nodes[0].len()
        }

        fn next_neighbor(&mut self) -> Result<usize> {
            let neighbors = self.nodes[self.current_level][self.current_node]
                .as_ref()
                .unwrap();
            if self.current_neighbor >= neighbors.len() {
                Ok(NO_MORE_DOCS as usize)
            } else {
                let result = neighbors[self.current_neighbor];
                self.current_neighbor += 1;
                Ok(result)
            }
        }

        fn num_levels(&self) -> Result<usize> {
            Ok(self.nodes.len())
        }

        fn entry_node(&self) -> Result<Option<usize>> {
            Ok(Some(0))
        }

        type NodeIterator = NodeIteratorImpl;

        fn get_nodes_on_level(&mut self, level: usize) -> Result<Self::NodeIterator> {
            let mut count = 0;
            for neighbors in &self.nodes[level] {
                if neighbors.is_some() {
                    count += 1;
                }
            }

            let final_count = count;
            let v = NodeIteratorImpl::new(self.nodes.clone(), final_count, level);
            Ok(v)
        }
    }
    impl std::fmt::Display for MockGraph {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            for level in (0..self.nodes.len()).rev() {
                writeln!(f, "\nLEVEL {}", level)?;
                for (node, neighbors) in self.nodes[level].iter().enumerate() {
                    if !neighbors.is_some() {
                        writeln!(f, "  {}: {:?}", node, neighbors)?;
                    }
                }
            }
            Ok(())
        }
    }

    pub struct NodeIteratorImpl {
        cur: i32,
        cur_count: i32,
        final_count: i32,
        level: usize,
        nodes: Vec<Vec<Option<Vec<usize>>>>,
        size: usize,
    }
    impl NodeIteratorImpl {
        pub fn new(nodes: Vec<Vec<Option<Vec<usize>>>>, final_count: i32, level: usize) -> Self {
            NodeIteratorImpl {
                cur: -1,
                cur_count: 0,
                level,
                final_count,
                nodes,
                size: final_count as usize,
            }
        }
    }

    impl Iterator for NodeIteratorImpl {
        type Item = usize;

        fn next(&mut self) -> Option<Self::Item> {
            if !self.has_next() {
                return None;
            }
            while self.cur_count < self.final_count {
                self.cur += 1;
                if self.nodes[self.level][self.cur as usize].is_some() {
                    self.cur_count += 1;
                    return Some(self.cur as usize);
                }
            }
            unreachable!()
        }
    }

    impl NodesIterator for NodeIteratorImpl {
        fn size(&self) -> usize {
            self.size
        }

        fn consume(&mut self, _dest: &mut [usize]) -> Option<usize> {
            unreachable!()
        }

        fn has_next(&self) -> bool {
            self.cur_count < self.final_count
        }
    }
}
