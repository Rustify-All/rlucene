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
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::term::Term;
use crate::core::index::terms::{Terms, terms_util};
use crate::core::search::collection_statistics::CollectionStatistics;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::search::term_statistics::TermStatistics;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

pub(crate) static MAX_CLAUSE_COUNT: AtomicI32 = AtomicI32::new(1024);
/// Thresholds for index slice allocation logic.
/// To change the default, extend IndexSearcher and use custom values
const MAX_DOCS_PER_SLICE: i32 = 250000;
const MAX_SEGMENTS_PER_SLICE: usize = 5;
pub struct IndexSearcher<IRC, S>
where
    IRC: IndexReaderContext,
    S: Similarity,
{
    reader_context: IRC,
    leaf_slices: Option<Vec<LeafSlice>>,
    similarity: Rc<S>,
    leaf_slices_init_lock: Mutex<()>,
}

impl<IRC, S> IndexSearcher<IRC, S>
where
    IRC: IndexReaderContext,
    S: Similarity,
{
    pub fn stored_fields(&self) {}

    /// Returns the leaf slices used for concurrent searching. Override [`slices()`](Self::slices) to customize how slices are created.
    pub fn get_slices(&mut self) -> Result<&[LeafSlice]> {
        if self.leaf_slices.is_none() {
            self.compute_and_cache_slices()?;
        }
        Ok(self.leaf_slices.as_ref().unwrap().as_slice())
    }
    fn compute_and_cache_slices(&mut self) -> Result<()> {
        let _guard = self.leaf_slices_init_lock.lock();
        if self.leaf_slices.is_none() {
            let res = slices(self.reader_context.leaves()?)?;
            // Enforce that there aren't multiple leaf partitions within the same leaf slice pointing to the
            // same leaf context. It is a requirement that [`Collector::get_leaf_collector(LeafReaderContext)`]
            // gets called once per leaf context.
            //
            // Also, it does not make sense to partition a segment to then search those partitions as part of
            // the same slice, because the goal of partitioning is parallel searching which happens at the
            // slice level.
            for leaf_slice in &res {
                if leaf_slice.partitions.len() <= 1 {
                    continue;
                }
                enforce_distinct_leaves(leaf_slice)?;
            }

            self.leaf_slices = Some(res);
        }
        Ok(())
    }
    pub fn get_top_reader_context(&self) -> &IRC {
        &self.reader_context
    }
    pub fn get_similarity(&self) -> Rc<S> {
        self.similarity.clone()
    }
    /// Returns [`TermStatistics`] for a term.
    ///
    /// This method can be overridden, for example, to return a term's statistics across
    /// a distributed collection.
    ///
    /// # Arguments
    ///
    /// * `doc_freq` — The document frequency of the term. It must be greater or equal to 1.
    /// * `total_term_freq` — The total term frequency.
    ///
    /// # Returns
    ///
    /// A [`TermStatistics`] (never `None`).
    ///
    /// **Lucene Experimental**
    pub fn term_statistics(
        &self,
        term: Arc<Term>,
        doc_freq: i32,
        total_term_freq: i64,
    ) -> Result<TermStatistics> {
        TermStatistics::new(term, doc_freq as i64, total_term_freq)
    }
    /// Returns [`CollectionStatistics`] for a field, or `None` if the field does not exist
    /// (has no indexed terms).
    ///
    ///
    /// This method can be overridden, for example, to return a field's statistics across
    /// a distributed collection.
    pub fn collection_statistics(&self, field: &str) -> Result<Option<CollectionStatistics>> {
        let mut doc_count: i64 = 0;
        let mut sum_total_term_freq: i64 = 0;
        let mut sum_doc_freq: i64 = 0;

        let v = self.reader_context.reader();
        for leaf in self.reader_context.leaves()? {
            let reader = leaf.reader();
            let terms = terms_util::get_terms(reader, field)?;
            doc_count += terms.get_doc_count()? as i64;
            sum_total_term_freq += terms.get_sum_total_term_freq()?;
            sum_doc_freq += terms.get_sum_doc_freq()?;
        }

        if doc_count == 0 {
            return Ok(None);
        }

        let stats = CollectionStatistics::new(
            field.to_string(),
            self.reader_context.reader().max_doc()? as i64,
            doc_count,
            sum_total_term_freq,
            sum_doc_freq,
        )?;

        Ok(Some(stats))
    }
}
/// Returns the maximum number of clauses permitted, `1024` by default.
///
/// Attempts to add more than the permitted number of clauses cause a [`TooManyClauses`] error to be thrown.
///
/// See also [`set_max_clause_count()`].
pub fn get_max_clause_count() -> i32 {
    MAX_CLAUSE_COUNT.load(Ordering::Relaxed)
}
/// Set the maximum number of clauses permitted per Query. Default value is 1024.
pub fn set_max_clause_count(value: i32) {
    MAX_CLAUSE_COUNT.store(value, Ordering::Relaxed);
}
pub fn do_slices<LR>(
    leaves: &[LeafReaderContext<LR>],
    max_docs_per_slice: i32,
    max_segments_per_slice: usize,
    allow_segment_partitions: bool,
) -> Result<Vec<LeafSlice>>
where
    LR: LeafReader,
{
    let mut ctx_map: HashMap<usize, &LeafReaderContext<LR>> = HashMap::with_capacity(leaves.len());
    let mut sorted_leaves: Vec<(usize, i32)> = Vec::with_capacity(leaves.len());

    for ctx in leaves {
        let ord = ctx.ord;
        let max_doc = ctx.reader().max_doc()?;
        ctx_map.insert(ord, ctx);
        sorted_leaves.push((ord, max_doc));
    }
    sorted_leaves.sort_by(|a, b| b.1.cmp(&a.1));

    if allow_segment_partitions {
        let mut grouped_leaf_partitions: Vec<Vec<LeafReaderContextPartition>> = Vec::new();
        let mut current_slice_num_docs = 0;
        let mut group: Option<Vec<LeafReaderContextPartition>> = None;

        for (ord, _) in sorted_leaves {
            let ctx = ctx_map[&ord];
            let ctx_max_doc = ctx.reader().max_doc()?;
            if ctx_max_doc > max_docs_per_slice {
                assert!(group.is_none());
                // if the segment does not fit in a single slice, we split it into maximum 5 partitions of equal size
                let num_slices = std::cmp::min(
                    5,
                    (ctx_max_doc + max_docs_per_slice - 1) / max_docs_per_slice,
                );
                let num_docs = ctx_max_doc / num_slices;
                let mut max_doc_id = num_docs;
                let mut min_doc_id = 0;

                for _ in 0..(num_slices - 1) {
                    grouped_leaf_partitions.push(vec![
                        LeafReaderContextPartition::create_from_and_to(
                            ctx, min_doc_id, max_doc_id,
                        )?,
                    ]);
                    min_doc_id = max_doc_id;
                    max_doc_id += num_docs;
                }
                // the last slice gets all the remaining docs
                grouped_leaf_partitions.push(vec![LeafReaderContextPartition::create_from_and_to(
                    ctx,
                    min_doc_id,
                    ctx_max_doc,
                )?]);
            } else {
                if group.is_none() {
                    group = Some(Vec::new());
                }
                let group_ref = group.as_mut().unwrap();
                group_ref.push(LeafReaderContextPartition::create_for_entire_segment(ctx)?);
                current_slice_num_docs += ctx_max_doc;
                // We only split a segment when it does not fit entirely in a slice. We don't partition
                // the
                // segment that makes the current slice (which holds multiple segments) go over
                // maxDocsPerSlice. This means that a slice either contains multiple entire segments, or a
                // single partition of a segment.
                if group_ref.len() >= max_segments_per_slice
                    || current_slice_num_docs > max_docs_per_slice
                {
                    grouped_leaf_partitions.push(group.take().unwrap());
                    current_slice_num_docs = 0;
                }
            }
        }

        if let Some(g) = group.take() {
            grouped_leaf_partitions.push(g);
        }

        return Ok(grouped_leaf_partitions
            .into_iter()
            .map(LeafSlice::new)
            .collect());
    }

    let mut grouped_leaves: Vec<Vec<usize>> = Vec::new();
    let mut doc_sum: i64 = 0;
    let mut group: Option<Vec<usize>> = None;

    for (ord, _) in sorted_leaves {
        let ctx = ctx_map[&ord];
        let ctx_max_doc = ctx.reader().max_doc()?;

        if ctx_max_doc > max_docs_per_slice {
            assert!(group.is_none());
            grouped_leaves.push(vec![ord]);
        } else {
            if group.is_none() {
                group = Some(Vec::new());
            }
            let group_ref = group.as_mut().unwrap();
            group_ref.push(ord);
            doc_sum += ctx_max_doc as i64;

            if group_ref.len() >= max_segments_per_slice || doc_sum > max_docs_per_slice as i64 {
                grouped_leaves.push(group.take().unwrap());
                doc_sum = 0;
            }
        }
    }

    if let Some(g) = group.take() {
        grouped_leaves.push(g);
    }

    let mut slices = Vec::new();

    for ords in grouped_leaves {
        let mut partitions = Vec::new();
        for ord in ords {
            let ctx = ctx_map[&ord];
            let partition = LeafReaderContextPartition::create_for_entire_segment(ctx)?;
            partitions.push(partition);
        }
        slices.push(LeafSlice::new(partitions));
    }
    Ok(slices)
}
/// Expert: Creates an array of [`LeafSlice`] each holding a subset of the given leaves.
/// Each [`LeafSlice`] is executed in a single thread.
///
/// By default, segments with more than [`MAX_DOCS_PER_SLICE`] will get their own thread.
///
///
/// It is possible to leverage intra-segment concurrency by splitting segments into multiple
/// partitions. Such behaviour is not enabled by default as there is still a performance penalty
/// for queries that require segment-level computation ahead of time, such as points/range queries.
///
/// This is an implementation limitation that we expect to improve in future releases,
/// see [the corresponding GitHub issue](https://github.com/apache/lucene/issues/13745).
pub fn slices<LR>(leaves: &[LeafReaderContext<LR>]) -> Result<Vec<LeafSlice>>
where
    LR: LeafReader,
{
    do_slices(leaves, MAX_DOCS_PER_SLICE, MAX_SEGMENTS_PER_SLICE, false)
}

fn enforce_distinct_leaves(leaf_slice: &LeafSlice) -> Result<()> {
    let mut distinct_leaves = HashSet::new();

    for partition in &leaf_slice.partitions {
        if !distinct_leaves.insert(partition.ctx) {
            return Err(LuceneError::illegal_state(
                "The same slice targets multiple leaf partitions of the same leaf reader context. \
                A physical segment should rather get partitioned to be searched concurrently from \
                as many slices as the number of leaf partitions it is split into.",
            ));
        }
    }

    Ok(())
}
/// Thrown when an attempt is made to add more than [`get_max_clause_count()`] clauses.
///
/// This typically happens if a [`PrefixQuery`], [`FuzzyQuery`], [`WildcardQuery`],
/// or [`TermRangeQuery`] is expanded to many terms during search.
pub struct TooManyClauses;
impl TooManyClauses {
    pub fn new() -> LuceneError {
        Self::with_msg(format!(
            "maxClauseCount is set to {}",
            get_max_clause_count()
        ))
    }
    pub fn with_msg(msg: String) -> LuceneError {
        LuceneError::too_many_clauses(msg)
    }
}
pub struct TooManyNestedClauses;
impl TooManyNestedClauses {
    pub fn new() -> LuceneError {
        LuceneError::too_many_nested_clauses(format!(
            "Query contains too many nested clauses; maxClauseCount is set to {}",
            get_max_clause_count()
        ))
    }
}

/// Holds information about a specific leaf context and the corresponding range of doc ids to
/// search within. Used to optionally search across partitions of the same segment concurrently.
///
/// A partition instance can be created via [`LeafReaderContextPartition::create_for_entire_segment`],
/// in which case it will target the entire provided [`LeafReaderContext`].
/// A true partition of a segment can be created via
/// [`LeafReaderContextPartition::create_from_and_to`] providing the minimum doc id (inclusive) to
/// search as well as the max doc id (exclusive).
pub struct LeafReaderContextPartition {
    pub min_doc_id: i32,
    pub max_doc_id: i32,
    pub ctx: usize,
    pub doc_base: i32,
    pub ctx_max_doc: i32,
    // we keep track of maxDocs separately because we use NO_MORE_DOCS as upper bound when targeting
    // the entire segment. We use this only in tests.
    max_docs: i32,
}
impl LeafReaderContextPartition {
    pub fn new<LR>(
        leaf_reader_context: &LeafReaderContext<LR>,
        min_doc_id: i32,
        max_doc_id: i32,
        max_docs: i32,
    ) -> Result<Self>
    where
        LR: LeafReader,
    {
        let ctx_max_doc = leaf_reader_context.reader().max_doc()?;
        if min_doc_id >= max_doc_id {
            return Err(LuceneError::illegal_argument(format!(
                "minDocId is greater than or equal to maxDocId: [{}] >= [{}]",
                min_doc_id, max_doc_id
            )));
        }
        if min_doc_id < 0 {
            return Err(LuceneError::illegal_argument(format!(
                "minDocId is lower than 0: [{}]",
                min_doc_id
            )));
        }
        if min_doc_id >= ctx_max_doc {
            return Err(LuceneError::illegal_argument(format!(
                "minDocId is greater than maxDoc: [{}] >= [{}]",
                min_doc_id, ctx_max_doc
            )));
        }

        Ok(Self {
            min_doc_id,
            max_doc_id,
            ctx_max_doc,
            ctx: leaf_reader_context.ord,
            doc_base: leaf_reader_context.doc_base,
            max_docs,
        })
    }
    /// Creates a partition of the provided leaf context that targets the entire segment
    pub fn create_for_entire_segment<LR>(ctx: &LeafReaderContext<LR>) -> Result<Self>
    where
        LR: LeafReader,
    {
        Self::new(ctx, 0, NO_MORE_DOCS, ctx.reader().max_doc()?)
    }

    /// Creates a partition of the provided leaf context that targets a subset of the entire segment,
    /// starting from and including the min doc id provided, until and not including the provided max doc id
    pub fn create_from_and_to<LR>(
        ctx: &LeafReaderContext<LR>,
        min_doc_id: i32,
        max_doc_id: i32,
    ) -> Result<Self>
    where
        LR: LeafReader,
    {
        debug_assert!(max_doc_id != NO_MORE_DOCS);
        Self::new(ctx, min_doc_id, max_doc_id, max_doc_id - min_doc_id)
    }
}
/// A class holding a subset of the [`IndexSearcher`]’s leaf contexts to be executed within a
/// single thread. A leaf slice holds references to one or more [`LeafReaderContextPartition`]
/// instances. Each partition targets a specific doc id range of a [`LeafReaderContext`].
pub struct LeafSlice {
    /// The leaves that make up this slice.
    pub partitions: Vec<LeafReaderContextPartition>,

    max_docs: i32,
}

impl LeafSlice {
    pub fn new(mut partitions: Vec<LeafReaderContextPartition>) -> Self {
        partitions.sort_by(|a, b| {
            let doc_base_cmp = a.doc_base.cmp(&b.doc_base);
            if doc_base_cmp == std::cmp::Ordering::Equal {
                a.min_doc_id.cmp(&b.min_doc_id)
            } else {
                doc_base_cmp
            }
        });
        let max_docs = partitions.iter().map(|p| p.max_docs).sum();

        Self {
            partitions,
            max_docs,
        }
    }
    /// Returns the total number of docs that a slice targets,
    /// by summing the number of docs that each of its leaf context partitions targets.
    pub fn max_docs(&self) -> i32 {
        self.max_docs
    }
}
