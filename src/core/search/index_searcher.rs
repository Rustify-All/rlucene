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
use crate::core::index::query_timeout::QueryTimeout;
use crate::core::index::sort::Sort;
use crate::core::index::term::Term;
use crate::core::index::terms::{Terms, terms_util};
use crate::core::search::QueryCache;
use crate::core::search::bulk_scorer::BulkScorer;
use crate::core::search::collection_statistics::CollectionStatistics;
use crate::core::search::collector::Collector;
use crate::core::search::collector_manager::CollectorManager;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::search::field_doc::FieldDoc;
use crate::core::search::leaf_collector::LeafCollector;
use crate::core::search::query::{Query, QueryBase};
use crate::core::search::query_caching_policy::QueryCachingPolicy;
use crate::core::search::score_doc::ScoreDoc;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer_supplier::ScorerSupplier;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::search::term_statistics::TermStatistics;
use crate::core::search::time_limiting_bulk_scorer::TimeLimitingBulkScorer;
use crate::core::search::top_docs::TopDocs;
use crate::core::search::top_field_collector_manager::TopFieldCollectorManager;
use crate::core::search::top_field_docs::TopFieldDocs;
use crate::core::search::top_score_doc_collector_manager::TopScoreDocCollectorManager;
use crate::core::search::weight::{Either2Weight, Weight};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

pub(crate) static MAX_CLAUSE_COUNT: AtomicI32 = AtomicI32::new(1024);
const TOTAL_HITS_THRESHOLD: i32 = 1000;
/// Thresholds for index slice allocation logic.
/// To change the default, extend IndexSearcher and use custom values
const MAX_DOCS_PER_SLICE: i32 = 250000;
const MAX_SEGMENTS_PER_SLICE: usize = 5;
pub struct IndexSearcher<IRC, S, QT, QCP, QC>
where
    IRC: IndexReaderContext,
    S: Similarity,
    QT: QueryTimeout,
    QCP: QueryCachingPolicy,
    QC: QueryCache,
{
    reader_context: IRC,
    leaf_slices: Option<Arc<Vec<LeafSlice>>>,
    similarity: Arc<S>,
    leaf_slices_init_lock: Mutex<()>,
    query_timeout: Option<QT>,
    query_caching_policy: Arc<QCP>,
    query_cache: Arc<QC>,
    // partialResult may be set on one of the threads of the executor. It may be correct to not make
    // this variable volatile since joining these threads should ensure a happens-before relationship
    // that guarantees that writes become visible on the main thread, but making the variable volatile
    // shouldn't hurt either.
    partial_result: AtomicBool,
}

impl<IRC, S, QT, QCP, QC> IndexSearcher<IRC, S, QT, QCP, QC>
where
    IRC: IndexReaderContext,
    S: Similarity,
    QT: QueryTimeout,
    QCP: QueryCachingPolicy,
    QC: QueryCache,
{
    pub fn stored_fields(&self) {}

    /// Returns the leaf slices used for concurrent searching. Override [`slices()`](Self::slices) to customize how slices are created.
    pub fn get_slices_ref(&mut self) -> Result<&[LeafSlice]> {
        self.ensure_slices()?;
        Ok(self.leaf_slices.as_ref().unwrap().as_slice())
    }

    pub fn get_slices(&mut self) -> Result<Arc<Vec<LeafSlice>>> {
        self.ensure_slices()?;
        Ok(self.leaf_slices.as_ref().unwrap().clone())
    }
    fn ensure_slices(&mut self) -> Result<()> {
        if self.leaf_slices.is_none() {
            self.compute_and_cache_slices()?;
        }
        Ok(())
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

            self.leaf_slices = Some(Arc::new(res));
        }
        Ok(())
    }

    pub fn search_after_score(
        &mut self,
        after: Option<ScoreDoc>,
        query: Query,
        num_hits: i32,
    ) -> Result<TopDocs<ScoreDoc>> {
        let limit = std::cmp::max(1, self.reader_context.reader().max_doc()?);

        if let Some(ref a) = after
            && a.doc >= limit
        {
            return Err(LuceneError::illegal_argument(format!(
                "after.doc exceeds the number of documents in the reader: after.doc={} limit={}",
                a.doc, limit
            )));
        }

        let capped_num_hits = std::cmp::min(num_hits, limit);
        let manager =
            TopScoreDocCollectorManager::with_after(capped_num_hits, after, TOTAL_HITS_THRESHOLD)?;

        self.search_with_collector_manager(query, &manager)
    }
    pub fn search(&mut self, query: Query, n: i32) -> Result<TopDocs<ScoreDoc>> {
        self.search_after_score(None, query, n)
    }
    pub fn get_top_reader_context(&self) -> &IRC {
        &self.reader_context
    }
    pub fn get_similarity(&self) -> Arc<S> {
        self.similarity.clone()
    }
    pub fn search_after_field_with_score(
        &mut self,
        after: Option<FieldDoc>,
        query: Query,
        num_hits: i32,
        sort: Sort,
        do_doc_scores: bool,
    ) -> Result<TopFieldDocs> {
        self.do_search_after_field(after, query, num_hits, sort, do_doc_scores)
    }
    pub fn search_after_field(
        &mut self,
        after: Option<FieldDoc>,
        query: Query,
        num_hits: i32,
        sort: Sort,
    ) -> Result<TopFieldDocs> {
        self.do_search_after_field(after, query, num_hits, sort, false)
    }

    fn do_search_after_field(
        &mut self,
        after: Option<FieldDoc>,
        query: Query,
        num_hits: i32,
        sort: Sort,
        do_doc_scores: bool,
    ) -> Result<TopFieldDocs> {
        let limit = std::cmp::max(1, self.reader_context.reader().max_doc()?);

        if let Some(ref a) = after
            && a.base.doc >= limit
        {
            return Err(LuceneError::illegal_argument(format!(
                "after.doc exceeds the number of documents in the reader: after.doc={} limit={}",
                a.base.doc, limit
            )));
        }

        let capped_num_hits = std::cmp::min(num_hits, limit);
        // TODO: IMPORTANT
        // let rewritten_sort = sort.rewrite(self)?;
        let sort = Rc::new(sort);
        let manager = TopFieldCollectorManager::new_with_after(
            sort,
            capped_num_hits,
            after,
            TOTAL_HITS_THRESHOLD,
        )?;

        let top_field_docs = self.search_with_collector_manager(query, &manager)?;

        if do_doc_scores {
            // TopFieldCollector::populate_scores(&mut top_field_docs.score_docs, self, &query)?;
        }

        Ok(top_field_docs)
    }

    pub fn search_with_collector_manager<CM>(
        &mut self,
        mut query: Query,
        collector_manager: &CM,
    ) -> Result<CM::T>
    where
        CM: CollectorManager,
    {
        let first_collector = collector_manager.new_collector()?;
        let needs_scores = first_collector.score_mode().needs_scores();
        query = Self::rewrite_if_needed_scores(query, needs_scores)?;
        let score_mode = first_collector.score_mode();
        let weight = Arc::new(self.create_weight(query, score_mode, 1.0)?);
        self.search_with_first_collector(weight, collector_manager, first_collector)
    }
    fn search_with_first_collector<W, CM>(
        &mut self,
        weight: Arc<W>,
        collector_manager: &CM,
        first_collector: CM::C,
    ) -> Result<CM::T>
    where
        W: Weight<IRC::LeafReader>,
        CM: CollectorManager,
    {
        let leaf_slices = self.get_slices()?;
        if leaf_slices.is_empty() {
            debug_assert!(self.reader_context.leaves()?.is_empty());
            collector_manager.reduce(vec![first_collector])
        } else {
            let mut collectors = Vec::with_capacity(leaf_slices.len());
            let score_mode = first_collector.score_mode();
            collectors.push(Some(first_collector));
            for _ in 1..leaf_slices.len() {
                let collector = collector_manager.new_collector()?;
                if score_mode != collector.score_mode() {
                    return Err(LuceneError::illegal_state(
                        "CollectorManager does not always produce collectors with the same score mode",
                    ));
                }
                collectors.push(Some(collector));
            }
            let mut list_tasks = Vec::with_capacity(leaf_slices.len());
            // TODO: IMPORTANT： 这里需要使用多线程,但是设计较大改动 暂时不懂
            for i in 0..leaf_slices.len() {
                let leaves = leaf_slices[i].partitions.as_slice();
                let mut collector = collectors[i].take().unwrap();
                self.search_partitions(leaves, weight.clone(), &mut collector)?;
                list_tasks.push(collector)
            }
            collector_manager.reduce(list_tasks)
        }
    }

    pub(crate) fn search_partitions<W, C>(
        &self,
        partitions: &[LeafReaderContextPartition],
        weight: Arc<W>,
        collector: &mut C,
    ) -> Result<()>
    where
        W: Weight<IRC::LeafReader>,
        C: Collector,
    {
        // we pass `Weight` to `Collector` via parameter in Rust Lucene
        // collector.set_weight(weight)?;

        for partition in partitions {
            self.search_leaf(
                partition.ctx,
                partition.min_doc_id,
                partition.max_doc_id,
                weight.as_ref(),
                collector,
            )?;
        }

        Ok(())
    }
    pub(crate) fn search_leaf<W, C>(
        &self,
        ctx_ord: usize,
        min_doc_id: i32,
        max_doc_id: i32,
        weight: &W,
        collector: &mut C,
    ) -> Result<()>
    where
        W: Weight<IRC::LeafReader>,
        C: Collector,
    {
        let ctx = &self.reader_context.leaves()?[ctx_ord];
        let mut leaf_collector = match collector.get_leaf_collector(ctx, Some(weight)) {
            Ok(leaf_collector) => leaf_collector,
            Err(LuceneError::CollectionTerminated(_)) => {
                // there is no doc of interest in this reader context
                // continue with the following leaf
                return Ok(());
            },
            Err(e) => return Err(e),
        };

        if let Some(mut scorer_supplier) = weight.scorer_supplier(ctx)? {
            scorer_supplier.set_top_level_scoring_clause()?;
            let mut scorer = scorer_supplier.bulk_scorer(ctx)?;
            let bits = ctx.reader().get_live_docs()?;
            let result: Result<()> = (|| {
                let _ = match self.query_timeout {
                    None => {
                        scorer.score(&mut leaf_collector, bits.as_ref(), min_doc_id, max_doc_id)?
                    },
                    Some(ref qt) => {
                        let mut scorer = TimeLimitingBulkScorer::new(scorer, qt);
                        scorer.score(&mut leaf_collector, bits.as_ref(), min_doc_id, max_doc_id)?
                    },
                };
                Ok(())
            })();

            match result {
                Ok(_) => {},
                Err(LuceneError::CollectionTerminated(_)) => {
                    // collection was terminated prematurely
                    // continue with the following leaf
                },
                Err(LuceneError::TimeExceeded(_)) => {
                    self.partial_result.store(true, Ordering::Relaxed);
                },
                Err(e) => return Err(e),
            }
        }
        // Note: this is called if collection ran successfully, including the above special cases of
        // CollectionTerminatedException and TimeExceededException, but no other exception.
        leaf_collector.finish()?;
        Ok(())
    }
    pub(crate) fn rewrite_if_needed_scores(original: Query, _needs_scores: bool) -> Result<Query> {
        // TODO
        Ok(original)
    }
    pub(crate) fn create_weight<Q>(
        &self,
        query: Q,
        score_mode: ScoreMode,
        boost: f32,
    ) -> Result<WeightEnum<Q, S, IRC, QCP, QC>>
    where
        Q: QueryBase,
    {
        let weight = query.create_weight(self, &score_mode, boost, None)?;
        let v = if !score_mode.needs_scores() {
            Either2Weight::A(
                self.query_cache
                    .do_cache(weight, self.query_caching_policy.clone()),
            )
        } else {
            Either2Weight::B(weight)
        };
        Ok(v)
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
pub type WeightEnum<Q, S, IRC, QCP, QC> = Either2Weight<
    <QC as QueryCache>::Weight<
        <Q as QueryBase>::Weight<S, IRC, QCP, QC>,
        QCP,
        <IRC as IndexReaderContext>::LeafReader,
    >,
    <Q as QueryBase>::Weight<S, IRC, QCP, QC>,
>;

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
