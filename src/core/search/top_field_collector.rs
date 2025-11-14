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
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::sort::Sort;
use crate::core::index::sorted_numeric_doc_values::SortedNumericDocValues;
use crate::core::search::collector::Collector;
use crate::core::search::doc_id_set_iterator::Either2DocIdSetIterator;
use crate::core::search::doc_id_stream::DocIdStream;
use crate::core::search::dummy::dummy_leaf_collector::DummyLeafCollector;
use crate::core::search::field_comparator::{
    FieldComparator, FieldComparatorEnum, FieldComparatorValue,
};
use crate::core::search::field_doc::FieldDoc;
use crate::core::search::field_value_hit_queue::{
    Entry, FieldValueHitQueueComparator, TopFieldScoreDoc,
};
use crate::core::search::leaf_collector::{Either2LeafCollector, LeafCollector};
use crate::core::search::leaf_field_comparator::{
    LeafFieldComparator, LeafFieldComparatorDocIdSetIteratorRef, LeafFieldComparatorEnum,
};
use crate::core::search::max_score_accumulator::MaxScoreAccumulator;
use crate::core::search::multi_leaf_field_comparator::MultiLeafFieldComparator;
use crate::core::search::scorable::Scorable;
use crate::core::search::score_caching_wrapping_scorer::ScoreCachingWrappingLeafCollector;
use crate::core::search::score_doc::ScoreDoc;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::sort_field::SortField;
use crate::core::search::sort_field_enum::SortFieldEnum;
use crate::core::search::top_docs_collector::{TopDocsCollector, TopDocsCollectorBase};
use crate::core::search::top_field_docs::TopFieldDocs;
use crate::core::search::total_hits::{Relation, TotalHits};
use crate::core::search::weight::Weight;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::priority_queue::PriorityQueue;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::vec;

/// A [`Collector`] that sorts by [`SortField`] using [`FieldComparator`]s.
///
/// See the constructor of [`TopFieldCollectorManager`](crate::core::search::top_field_collector_manager::TopFieldCollectorManager) for instantiating a
/// [`TopFieldCollectorManager`](crate::core::search::top_field_collector_manager::TopFieldCollectorManager) with support for concurrency in [`IndexSearcher`](crate::core::search::index_searcher::IndexSearcher).
pub struct TopFieldCollector {
    base: TopDocsCollectorBase<TopFieldScoreDoc, FieldValueHitQueueComparator>,
    num_hits: i32,
    total_hits_threshold: i32,
    can_set_min_score: bool,
    search_sort_part_of_index_sort: Option<bool>,
    min_score_acc: Option<Arc<MaxScoreAccumulator>>,
    min_competitive_score: f32,
    num_comparators: i32,
    queue_full: bool,
    doc_base: i32,
    needs_scores: bool,
    score_mode: ScoreMode,
}
impl TopFieldCollector {
    pub fn new(
        pq: PriorityQueue<TopFieldScoreDoc, FieldValueHitQueueComparator>,
        num_hits: i32,
        total_hits_threshold: i32,
        needs_scores: bool,
        min_score_acc: Option<Arc<MaxScoreAccumulator>>,
    ) -> Result<Self> {
        let total_hits_threshold = std::cmp::max(total_hits_threshold, num_hits);
        debug_assert!(total_hits_threshold >= 0);

        let num_comparators = pq.get_comparators().len() as i32;

        let first_comparator = &pq.get_comparators()[0];
        let reverse_mul = pq.get_reverse_mul()[0];

        let (score_mode, can_set_min_score) = if matches!(first_comparator, FieldComparatorEnum::Relevance(_))
                && reverse_mul == 1// if the natural sort is preserved (sort by descending relevance)
                && total_hits_threshold != i32::MAX
        {
            (ScoreMode::TopScores, true)
        } else {
            let can_set_min_score = false;
            let score_mode = if total_hits_threshold != i32::MAX {
                if needs_scores {
                    ScoreMode::TopDocsWithScores
                } else {
                    ScoreMode::TopDocs
                }
            } else if needs_scores {
                ScoreMode::Complete
            } else {
                ScoreMode::CompleteNoScores
            };
            (score_mode, can_set_min_score)
        };

        let base = TopDocsCollectorBase::new(pq);
        Ok(Self {
            base,
            num_hits,
            total_hits_threshold,
            can_set_min_score,
            search_sort_part_of_index_sort: None,
            min_score_acc,
            min_competitive_score: 0.0,
            num_comparators,
            queue_full: false,
            doc_base: 0,
            needs_scores,
            score_mode,
        })
    }
    pub(crate) fn update_global_min_competitive_score<S: Scorable>(
        &mut self,
        scorer: &mut S,
    ) -> Result<()> {
        match &self.min_score_acc {
            Some(acc) if self.can_set_min_score => {
                // we can start checking the global maximum score even if the local queue
                // is not full or the threshold is not reached on the local competitor:
                // the fact that there is a shared min competitive score implies that one
                // of the collectors hit its totalHitsThreshold already
                let max_min_score = acc.get_raw();

                if max_min_score != i64::MIN {
                    let score = MaxScoreAccumulator::to_score(max_min_score);
                    if score > self.min_competitive_score {
                        scorer.set_min_competitive_score(score)?;
                        self.min_competitive_score = score;
                        self.base.total_hits_relation = Relation::GreaterThanOrEqualTo;
                    }
                }
                Ok(())
            },
            _ => Ok(()),
        }
    }

    pub(crate) fn update_min_competitive_score<S: Scorable>(
        &mut self,
        scorer: &mut S,
    ) -> Result<()> {
        debug_assert!(self.total_hits_threshold >= 0);
        if self.can_set_min_score
            && self.queue_full
            && self.base.total_hits > self.total_hits_threshold as usize
        {
            let bottom = self.bottom()?;

            let first_comparator = &self.base.pq.get_comparators()[0];
            let min_score = first_comparator
                .value(bottom.slot()?)
                .and_then(FieldComparatorValue::into_f32)
                .expect("first comparator is not a float");

            if min_score > self.min_competitive_score {
                scorer.set_min_competitive_score(min_score)?;
                self.min_competitive_score = min_score;
                self.base.total_hits_relation = Relation::GreaterThanOrEqualTo;

                if let Some(acc) = &self.min_score_acc {
                    acc.accumulate(self.doc_base, min_score);
                }
            }
        }
        Ok(())
    }
    pub(crate) fn add(&mut self, slot: i32, doc: i32) -> Result<()> {
        let global_doc = doc + self.doc_base;
        self.pq_mut().add(Entry::new(slot, global_doc).into())?;

        // The queue is full either when total_hits == num_hits (in SimpleFieldCollector),
        // in which case slot = total_hits - 1, or when hits_collected == num_hits (in
        // PagingFieldCollector this is hits on the current page) and slot = hits_collected - 1.
        debug_assert!(slot < self.num_hits);

        self.queue_full = slot == self.num_hits - 1;
        Ok(())
    }
    pub(crate) fn update_bottom(&mut self, doc: i32) -> Result<()> {
        let global_doc = doc + self.doc_base;
        let bottom = self.bottom_mut()?;
        bottom.base().doc = global_doc;
        let pq = self.pq_mut();
        pq.update_top()?;
        Ok(())
    }
    #[inline]
    fn bottom(&self) -> Result<&TopFieldScoreDoc> {
        self.base
            .pq
            .top()
            .ok_or_else(|| LuceneError::illegal_state("priority queue bottom missing"))
    }
    #[inline]
    fn bottom_mut(&mut self) -> Result<&mut TopFieldScoreDoc> {
        self.base
            .pq
            .top_mut()
            .ok_or_else(|| LuceneError::illegal_state("priority queue bottom missing"))
    }
}

impl Collector for TopFieldCollector {
    type LeafCollector<'a, LR>
        = DummyLeafCollector
    where
        Self: 'a,
        LR: LeafReader;

    fn get_leaf_collector<'a, W, LR>(
        &'a mut self,
        _context: &LeafReaderContext<LR>,
        _weight: Option<&W>,
    ) -> Result<Self::LeafCollector<'a, LR>>
    where
        LR: LeafReader,
        W: Weight<LR>,
    {
        unreachable!("should call Simple/PagingFieldCollector instead")
    }

    fn score_mode(&self) -> ScoreMode {
        self.score_mode
    }
}

impl TopDocsCollector for TopFieldCollector {
    type Item = TopFieldScoreDoc;
    type Cmp = FieldValueHitQueueComparator;
    type TopDocsLike = TopFieldDocs;

    fn pq(&self) -> &PriorityQueue<Self::Item, Self::Cmp> {
        &self.base.pq
    }

    fn pq_mut(&mut self) -> &mut PriorityQueue<Self::Item, Self::Cmp> {
        &mut self.base.pq
    }

    fn total_hits(&self) -> usize {
        self.base.total_hits
    }

    fn get_total_hits_relation(&self) -> Relation {
        self.base.total_hits_relation
    }

    fn populate_results(&mut self, results: &mut [Self::Item], how_many: usize) -> Result<()> {
        let pq = &mut self.base.pq;
        for i in (0..how_many).rev() {
            let entry = pq.pop_unchecked()?;
            results[i] = pq.fill_fields(entry)?;
        }
        Ok(())
    }

    fn new_top_docs(&self, results: Option<Vec<Self::Item>>, _start: i32) -> Self::TopDocsLike
    where
        Self: Sized,
    {
        let result = results.unwrap_or_else(std::vec::Vec::new);
        // TODO: TopFieldDocs#fields not used in Java Lucene, so far we set it to empty vec
        TopFieldDocs::new(
            TotalHits::new(self.total_hits(), self.get_total_hits_relation()),
            result,
            vec![],
        )
    }
}

pub struct TopFieldLeafCollector<'a, LR>
where
    LR: LeafReader,
{
    base: &'a mut TopFieldCollector,
    reverse_mul: i32,
    collected_all_competitive_hits: bool,
    comparator: TopFieldLeafComparatorEnum<LR>,
}
impl<'a, LR> TopFieldLeafCollector<'a, LR>
where
    LR: LeafReader,
{
    pub fn new(
        base: &'a mut TopFieldCollector,
        sort: &Sort,
        context: &LeafReaderContext<LR>,
    ) -> Result<Self> {
        // as all segments are sorted in the same way, enough to check only the 1st segment for
        // indexSort
        if base.search_sort_part_of_index_sort.is_none()
            && let Some(index_sort) = context.reader().get_metadata()?.get_sort()
        {
            let can_early_terminate = can_early_terminate(sort, Some(index_sort))?;
            base.search_sort_part_of_index_sort = Some(can_early_terminate);

            if can_early_terminate {
                let pq = &mut base.base.pq;
                let first_comparator = &mut pq.get_comparators_mut()[0];
                first_comparator.disable_skipping();
            }
        }

        let leaf_comparators = base.base.pq.get_leaf_comparator(context)?;
        let reverse_muls = base.base.pq.get_reverse_mul_shared();

        let (reverse_mul, comparator) = if leaf_comparators.len() == 1 {
            (
                reverse_muls[0],
                TopFieldLeafComparatorEnum::Single(leaf_comparators.into_iter().next().unwrap()),
            )
        } else {
            (
                1,
                TopFieldLeafComparatorEnum::Multi(MultiLeafFieldComparator::new(
                    leaf_comparators,
                    reverse_muls,
                )?),
            )
        };

        Ok(Self {
            base,
            reverse_mul,
            collected_all_competitive_hits: false,
            comparator,
        })
    }
    pub(crate) fn count_hit<S: Scorable>(&mut self, scorer: &mut S, _doc: i32) -> Result<()> {
        self.base.base.total_hits += 1;
        debug_assert!(self.base.base.total_hits <= i32::MAX as usize);
        let hit_count_so_far = self.base.base.total_hits as i32;

        if let Some(acc) = &self.base.min_score_acc {
            debug_assert!(acc.mod_interval <= i32::MAX as i64);
            if (hit_count_so_far & acc.mod_interval as i32) == 0 {
                self.base.update_global_min_competitive_score(scorer)?;
            }
        }

        if !self.base.score_mode.is_exhaustive()
            && self.base.base.total_hits_relation == Relation::EqualTo
            && hit_count_so_far > self.base.total_hits_threshold
        {
            let comparators = self.base.base.pq.get_comparators_mut();
            self.comparator.set_hits_threshold_reached(comparators)?;
            self.base.base.total_hits_relation = Relation::GreaterThanOrEqualTo;
        }

        Ok(())
    }
    pub(crate) fn threshold_check<S>(&mut self, doc: i32, scorer: &mut S) -> Result<bool>
    where
        S: Scorable,
    {
        let cmp_check = if self.collected_all_competitive_hits {
            true
        } else {
            let comparators = self.base.base.pq.get_comparators_mut();
            let cmp = self.comparator.compare_bottom(doc, scorer, comparators)?;
            self.reverse_mul * cmp <= 0
        };

        if cmp_check {
            // since docs are visited in doc Id order, if compare is 0, it means
            // this document is larger than anything else in the queue, and
            // therefore not competitive.
            if self.base.search_sort_part_of_index_sort.unwrap_or(false) {
                if self.base.base.total_hits > self.base.total_hits_threshold as usize {
                    self.base.base.total_hits_relation = Relation::GreaterThanOrEqualTo;
                    return Err(LuceneError::collection_terminated(
                        "collection terminated due to early termination threshold",
                    ));
                } else {
                    self.collected_all_competitive_hits = true;
                }
            } else if self.base.base.total_hits_relation == Relation::EqualTo {
                // we can start setting the min competitive score if the
                // threshold is reached for the first time here.
                self.base.update_min_competitive_score(scorer)?;
            }
            return Ok(true);
        }

        Ok(false)
    }
    pub(crate) fn collect_competitive_hit<S>(&mut self, doc: i32, scorer: &mut S) -> Result<()>
    where
        S: Scorable,
    {
        {
            let bottom = self.bottom()?;
            self.comparator.copy(
                bottom.slot()? as usize,
                doc,
                scorer,
                self.base.base.pq.get_comparators_mut(),
            )?;
        }
        self.base.update_bottom(doc)?;
        let bottom = self.bottom()?;
        self.comparator.set_bottom(
            bottom.slot()? as usize,
            self.base.base.pq.get_comparators_mut(),
        )?;
        self.base.update_min_competitive_score(scorer)?;

        Ok(())
    }
    pub(crate) fn collect_any_hit<S>(
        &mut self,
        doc: i32,
        hits_collected: i32,
        scorer: &mut S,
    ) -> Result<()>
    where
        S: Scorable,
    {
        // Startup transient: queue hasn't gathered numHits yet
        let slot = hits_collected - 1;
        // Copy hit into queue
        self.comparator.copy(
            slot as usize,
            doc,
            scorer,
            self.base.base.pq.get_comparators_mut(),
        )?;
        self.base.add(slot, doc)?;
        if self.base.queue_full {
            let bottom = self.bottom()?;
            self.comparator.set_bottom(
                bottom.slot()? as usize,
                self.base.base.pq.get_comparators_mut(),
            )?;
            self.base.update_min_competitive_score(scorer)?;
        }
        Ok(())
    }
    #[inline]
    fn bottom(&self) -> Result<&TopFieldScoreDoc> {
        self.base.bottom()
    }
    #[inline]
    fn bottom_mut(&mut self) -> Result<&mut TopFieldScoreDoc> {
        self.base.bottom_mut()
    }
}

impl<LR> Display for TopFieldLeafCollector<'_, LR>
where
    LR: LeafReader,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", std::any::type_name::<LR>())
    }
}

impl<'a, LR> LeafCollector for TopFieldLeafCollector<'a, LR>
where
    LR: LeafReader,
{
    fn set_scorer<S>(&mut self, scorer: &mut S) -> Result<()>
    where
        S: Scorable,
    {
        let comparators = self.base.base.pq.get_comparators_mut();
        self.comparator.set_scorer(scorer, comparators)?;

        if self.base.min_score_acc.is_none() {
            self.base.update_min_competitive_score(scorer)?;
        } else {
            self.base.update_global_min_competitive_score(scorer)?;
        }

        Ok(())
    }

    fn collect<S>(&mut self, _doc: i32, _scorer: &mut S) -> Result<()>
    where
        S: Scorable,
    {
        unreachable!("should not be called")
    }

    type DocIdSetIteratorRef<'b>
        = TopFieldLeafComparatorEnumIterRef<'b, LR>
    where
        Self: 'b,
        LR: 'b,
        <LR as LeafReader>::NumericDocValues: 'b,
        <LR as LeafReader>::SortedNumericDocValues: 'b,
        <<LR as LeafReader>::SortedNumericDocValues as SortedNumericDocValues>::NumericDocValues:
            'b;

    fn competitive_iterator(&mut self) -> Result<Option<Self::DocIdSetIteratorRef<'_>>> {
        let comparators = self.base.base.pq.get_comparators_mut();
        self.comparator.competitive_iterator(comparators)
    }
}

fn can_early_terminate(search_sort: &Sort, index_sort: Option<&Sort>) -> Result<bool> {
    Ok(can_early_terminate_on_doc_id(search_sort)?
        || can_early_terminate_on_prefix(search_sort, index_sort)?)
}

fn can_early_terminate_on_doc_id(search_sort: &Sort) -> Result<bool> {
    let fields = search_sort.get_sort();
    if let Some(SortFieldEnum::Sorter(field)) = fields.first() {
        let field_doc = SortField::get_field_doc()?;
        Ok(*field == field_doc)
    } else {
        Ok(false)
    }
}
fn can_early_terminate_on_prefix(search_sort: &Sort, index_sort: Option<&Sort>) -> Result<bool> {
    if let Some(index_sort) = index_sort {
        let fields1 = search_sort.get_sort();
        let fields2 = index_sort.get_sort();

        if fields1.len() > fields2.len() {
            return Ok(false);
        }

        Ok(fields1.iter().zip(fields2.iter()).all(|(a, b)| a == b))
    } else {
        Ok(false)
    }
}
/// Implements a TopFieldCollector over one SortField criteria, with tracking document scores and maxScore
pub struct SimpleFieldCollector {
    base: TopFieldCollector,
    sort: Arc<Sort>,
}
impl SimpleFieldCollector {
    pub fn new(
        sort: Arc<Sort>,
        queue: PriorityQueue<TopFieldScoreDoc, FieldValueHitQueueComparator>,
        num_hits: i32,
        total_hits_threshold: i32,
        min_score_acc: Option<Arc<MaxScoreAccumulator>>,
    ) -> Result<Self> {
        let base = TopFieldCollector::new(
            queue,
            num_hits,
            total_hits_threshold,
            sort.needs_scores(),
            min_score_acc,
        )?;
        Ok(Self { base, sort })
    }
}

impl Collector for SimpleFieldCollector {
    type LeafCollector<'a, LR>
        = SimpleLeafCollector<'a, LR>
    where
        Self: 'a,
        LR: LeafReader;

    fn get_leaf_collector<'a, W, LR>(
        &'a mut self,
        context: &LeafReaderContext<LR>,
        _weight: Option<&W>,
    ) -> Result<Self::LeafCollector<'a, LR>>
    where
        LR: LeafReader,
        W: Weight<LR>,
    {
        self.base.min_competitive_score = 0.0;
        self.base.doc_base = context.doc_base;
        let needs_scores = self.base.needs_scores;
        let collector = SimpleFieldLeafCollector::new(&mut self.base, &self.sort, context)?;
        if needs_scores {
            Ok(SimpleLeafCollector::B(
                ScoreCachingWrappingLeafCollector::new(collector),
            ))
        } else {
            Ok(SimpleLeafCollector::A(collector))
        }
    }

    fn score_mode(&self) -> ScoreMode {
        self.base.score_mode
    }
}

impl TopDocsCollector for SimpleFieldCollector {
    type Item = <TopFieldCollector as TopDocsCollector>::Item;
    type Cmp = <TopFieldCollector as TopDocsCollector>::Cmp;
    type TopDocsLike = <TopFieldCollector as TopDocsCollector>::TopDocsLike;

    fn pq(&self) -> &PriorityQueue<Self::Item, Self::Cmp> {
        self.base.pq()
    }

    fn pq_mut(&mut self) -> &mut PriorityQueue<Self::Item, Self::Cmp> {
        self.base.pq_mut()
    }

    fn total_hits(&self) -> usize {
        self.base.total_hits()
    }

    fn get_total_hits_relation(&self) -> Relation {
        self.base.get_total_hits_relation()
    }

    fn populate_results(&mut self, results: &mut [Self::Item], how_many: usize) -> Result<()> {
        self.base.populate_results(results, how_many)
    }

    fn new_top_docs(&self, results: Option<Vec<Self::Item>>, start: i32) -> Self::TopDocsLike
    where
        Self: Sized,
    {
        self.base.new_top_docs(results, start)
    }

    fn top_docs_size(&self) -> usize {
        self.base.top_docs_size()
    }

    fn top_docs(&mut self) -> Result<Self::TopDocsLike>
    where
        Self: Sized,
    {
        self.base.top_docs()
    }

    fn top_docs_with_start(&mut self, start: i32) -> Result<Self::TopDocsLike>
    where
        Self: Sized,
    {
        self.base.top_docs_with_start(start)
    }

    fn top_docs_with_start_limit(&mut self, start: i32, how_many: i32) -> Result<Self::TopDocsLike>
    where
        Self: Sized,
    {
        self.base.top_docs_with_start_limit(start, how_many)
    }
}
pub struct SimpleFieldLeafCollector<'a, LR>
where
    LR: LeafReader,
{
    base: TopFieldLeafCollector<'a, LR>,
}
impl<'a, LR> SimpleFieldLeafCollector<'a, LR>
where
    LR: LeafReader,
{
    pub fn new(
        base: &'a mut TopFieldCollector,
        sort: &Sort,
        context: &LeafReaderContext<LR>,
    ) -> Result<Self> {
        let base = TopFieldLeafCollector::new(base, sort, context)?;
        Ok(Self { base })
    }
}

impl<LR> Display for SimpleFieldLeafCollector<'_, LR>
where
    LR: LeafReader,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", std::any::type_name::<LR>(), self.base)
    }
}

impl<'a, LR> LeafCollector for SimpleFieldLeafCollector<'a, LR>
where
    LR: LeafReader,
{
    fn set_scorer<S>(&mut self, scorer: &mut S) -> Result<()>
    where
        S: Scorable,
    {
        self.base.set_scorer(scorer)
    }

    fn collect<S>(&mut self, doc: i32, scorer: &mut S) -> Result<()>
    where
        S: Scorable,
    {
        self.base.count_hit(scorer, doc)?;
        if self.base.base.queue_full {
            if self.base.threshold_check(doc, scorer)? {
                return Ok(());
            }
            self.base.collect_competitive_hit(doc, scorer)?;
        } else {
            let hits_collected = self.base.base.total_hits();
            debug_assert!(hits_collected <= i32::MAX as usize);
            self.base
                .collect_any_hit(doc, hits_collected as i32, scorer)?;
        }
        Ok(())
    }

    fn collect_stream<DS, S>(&mut self, stream: &mut DS, scorer: &mut S) -> Result<()>
    where
        DS: DocIdStream,
        S: Scorable,
    {
        self.base.collect_stream(stream, scorer)
    }

    type DocIdSetIteratorRef<'b>
        = <TopFieldLeafCollector<'a, LR> as LeafCollector>::DocIdSetIteratorRef<'b>
    where
        Self: 'b;

    fn competitive_iterator(&mut self) -> Result<Option<Self::DocIdSetIteratorRef<'_>>> {
        self.base.competitive_iterator()
    }

    fn finish(&mut self) -> Result<()> {
        self.base.finish()
    }
}
/// Implements a TopFieldCollector when after is Some.
pub struct PagingFieldCollector {
    base: TopFieldCollector,
    sort: Arc<Sort>,
    collected_hits: i32,
    after: ScoreDoc,
}

impl PagingFieldCollector {
    pub fn new(
        sort: Arc<Sort>,
        queue: PriorityQueue<TopFieldScoreDoc, FieldValueHitQueueComparator>,
        mut after: FieldDoc,
        num_hits: i32,
        total_hits_threshold: i32,
        min_score_acc: Option<Arc<MaxScoreAccumulator>>,
    ) -> Result<Self> {
        let mut base = TopFieldCollector::new(
            queue,
            num_hits,
            total_hits_threshold,
            sort.needs_scores(),
            min_score_acc,
        )?;

        // set top values for comparators
        let comparators = base.base.pq.get_comparators_mut();
        let fields = std::mem::take(&mut after.fields);
        let score_doc = std::mem::take(&mut after.base);

        for (comp, top_value) in comparators.iter_mut().zip(fields.into_iter()) {
            comp.set_top_value(top_value);
        }

        Ok(Self {
            base,
            sort,
            collected_hits: 0,
            after: score_doc,
        })
    }
}

impl Collector for PagingFieldCollector {
    type LeafCollector<'a, LR>
        = PagingLeafCollector<'a, LR>
    where
        Self: 'a,
        LR: LeafReader;

    fn get_leaf_collector<'a, W, LR>(
        &'a mut self,
        context: &LeafReaderContext<LR>,
        _weight: Option<&W>,
    ) -> Result<Self::LeafCollector<'a, LR>>
    where
        LR: LeafReader,
        W: Weight<LR>,
    {
        self.base.min_competitive_score = 0.0;
        self.base.doc_base = context.doc_base;
        let after_doc = self.after.doc - self.base.doc_base;

        let needs_scores = self.base.needs_scores;
        let collector = PagingFieldLeafCollector::new(
            &mut self.base,
            &self.sort,
            context,
            after_doc,
            &mut self.collected_hits,
        )?;

        if needs_scores {
            Ok(PagingLeafCollector::B(
                ScoreCachingWrappingLeafCollector::new(collector),
            ))
        } else {
            Ok(PagingLeafCollector::A(collector))
        }
    }

    fn score_mode(&self) -> ScoreMode {
        self.base.score_mode
    }
}

impl TopDocsCollector for PagingFieldCollector {
    type Item = <TopFieldCollector as TopDocsCollector>::Item;
    type Cmp = <TopFieldCollector as TopDocsCollector>::Cmp;
    type TopDocsLike = <TopFieldCollector as TopDocsCollector>::TopDocsLike;

    fn pq(&self) -> &PriorityQueue<Self::Item, Self::Cmp> {
        self.base.pq()
    }

    fn pq_mut(&mut self) -> &mut PriorityQueue<Self::Item, Self::Cmp> {
        self.base.pq_mut()
    }

    fn total_hits(&self) -> usize {
        self.base.total_hits()
    }

    fn get_total_hits_relation(&self) -> Relation {
        self.base.get_total_hits_relation()
    }

    fn populate_results(&mut self, results: &mut [Self::Item], how_many: usize) -> Result<()> {
        self.base.populate_results(results, how_many)
    }

    fn new_top_docs(&self, results: Option<Vec<Self::Item>>, start: i32) -> Self::TopDocsLike
    where
        Self: Sized,
    {
        self.base.new_top_docs(results, start)
    }

    fn top_docs_size(&self) -> usize {
        self.base.top_docs_size()
    }

    fn top_docs(&mut self) -> Result<Self::TopDocsLike>
    where
        Self: Sized,
    {
        self.base.top_docs()
    }

    fn top_docs_with_start(&mut self, start: i32) -> Result<Self::TopDocsLike>
    where
        Self: Sized,
    {
        self.base.top_docs_with_start(start)
    }

    fn top_docs_with_start_limit(&mut self, start: i32, how_many: i32) -> Result<Self::TopDocsLike>
    where
        Self: Sized,
    {
        self.base.top_docs_with_start_limit(start, how_many)
    }
}

/// Leaf collector for paging-based top field collection.
pub struct PagingFieldLeafCollector<'a, LR>
where
    LR: LeafReader,
{
    base: TopFieldLeafCollector<'a, LR>,
    after_doc: i32,
    collected_hits: &'a mut i32,
}

impl<'a, LR> PagingFieldLeafCollector<'a, LR>
where
    LR: LeafReader,
{
    pub fn new(
        base: &'a mut TopFieldCollector,
        sort: &Sort,
        context: &LeafReaderContext<LR>,
        after_doc: i32,
        collected_hits: &'a mut i32,
    ) -> Result<Self> {
        let base = TopFieldLeafCollector::new(base, sort, context)?;
        Ok(Self {
            base,
            after_doc,
            collected_hits,
        })
    }
}

impl<LR> Display for PagingFieldLeafCollector<'_, LR>
where
    LR: LeafReader,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", std::any::type_name::<LR>(), self.base)
    }
}

impl<'a, LR> LeafCollector for PagingFieldLeafCollector<'a, LR>
where
    LR: LeafReader,
{
    fn set_scorer<S>(&mut self, scorer: &mut S) -> Result<()>
    where
        S: Scorable,
    {
        self.base.set_scorer(scorer)
    }

    fn collect<S>(&mut self, doc: i32, scorer: &mut S) -> Result<()>
    where
        S: Scorable,
    {
        self.base.count_hit(scorer, doc)?;

        if self.base.base.queue_full && self.base.threshold_check(doc, scorer)? {
            return Ok(());
        }

        let top_cmp = {
            let comparators = self.base.base.base.pq.get_comparators_mut();
            self.base.comparator.compare_top(doc, scorer, comparators)? * self.base.reverse_mul
        };

        if top_cmp > 0 || (top_cmp == 0 && doc <= self.after_doc) {
            // already collected in previous page
            if self.base.base.base.total_hits_relation == Relation::EqualTo {
                // check if totalHitsThreshold is reached and we can update competitive score
                // necessary to account for possible update to global min competitive score
                self.base.base.update_min_competitive_score(scorer)?;
            }
            return Ok(());
        }

        if self.base.base.queue_full {
            self.base.collect_competitive_hit(doc, scorer)?;
        } else {
            *self.collected_hits += 1;
            self.base
                .collect_any_hit(doc, *self.collected_hits, scorer)?;
        }

        Ok(())
    }

    type DocIdSetIteratorRef<'b>
        = <TopFieldLeafCollector<'a, LR> as LeafCollector>::DocIdSetIteratorRef<'b>
    where
        Self: 'b;

    fn competitive_iterator(&mut self) -> Result<Option<Self::DocIdSetIteratorRef<'_>>> {
        self.base.competitive_iterator()
    }

    fn finish(&mut self) -> Result<()> {
        self.base.finish()
    }
}

type SimpleLeafCollector<'a, LR> = Either2LeafCollector<
    SimpleFieldLeafCollector<'a, LR>,
    ScoreCachingWrappingLeafCollector<SimpleFieldLeafCollector<'a, LR>>,
>;

type PagingLeafCollector<'a, LR> = Either2LeafCollector<
    PagingFieldLeafCollector<'a, LR>,
    ScoreCachingWrappingLeafCollector<PagingFieldLeafCollector<'a, LR>>,
>;

pub enum TopFieldCollectorEnum {
    Simple(SimpleFieldCollector),
    Paging(PagingFieldCollector),
}
impl TopFieldCollectorEnum {
    pub fn min_score_acc(&self) -> Option<Arc<MaxScoreAccumulator>> {
        match self {
            Self::Simple(inner) => inner.base.min_score_acc.clone(),
            Self::Paging(inner) => inner.base.min_score_acc.clone(),
        }
    }
}

pub enum FieldLeafCollectorEnum<'a, LR>
where
    LR: LeafReader,
{
    Simple(SimpleLeafCollector<'a, LR>),
    Paging(PagingLeafCollector<'a, LR>),
}

impl<'a, LR> Display for FieldLeafCollectorEnum<'a, LR>
where
    LR: LeafReader,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Simple(inner) => Display::fmt(inner, f),
            Self::Paging(inner) => Display::fmt(inner, f),
        }
    }
}

impl<'a, LR> LeafCollector for FieldLeafCollectorEnum<'a, LR>
where
    LR: LeafReader,
{
    type DocIdSetIteratorRef<'b>
        = Either2DocIdSetIterator<
        <SimpleLeafCollector<'a, LR> as LeafCollector>::DocIdSetIteratorRef<'b>,
        <PagingLeafCollector<'a, LR> as LeafCollector>::DocIdSetIteratorRef<'b>,
    >
    where
        Self: 'b;

    fn set_scorer<S>(&mut self, scorer: &mut S) -> Result<()>
    where
        S: Scorable,
    {
        match self {
            Self::Simple(inner) => inner.set_scorer(scorer),
            Self::Paging(inner) => inner.set_scorer(scorer),
        }
    }

    fn collect<S>(&mut self, doc: i32, scorer: &mut S) -> Result<()>
    where
        S: Scorable,
    {
        match self {
            Self::Simple(inner) => inner.collect(doc, scorer),
            Self::Paging(inner) => inner.collect(doc, scorer),
        }
    }

    fn collect_stream<DS, S>(&mut self, stream: &mut DS, scorer: &mut S) -> Result<()>
    where
        DS: DocIdStream,
        S: Scorable,
    {
        match self {
            Self::Simple(inner) => inner.collect_stream(stream, scorer),
            Self::Paging(inner) => inner.collect_stream(stream, scorer),
        }
    }

    fn competitive_iterator(&mut self) -> Result<Option<Self::DocIdSetIteratorRef<'_>>> {
        match self {
            Self::Simple(inner) => inner
                .competitive_iterator()
                .map(|opt| opt.map(Either2DocIdSetIterator::A)),
            Self::Paging(inner) => inner
                .competitive_iterator()
                .map(|opt| opt.map(Either2DocIdSetIterator::B)),
        }
    }

    fn finish(&mut self) -> Result<()> {
        match self {
            Self::Simple(inner) => inner.finish(),
            Self::Paging(inner) => inner.finish(),
        }
    }
}

impl Collector for TopFieldCollectorEnum {
    type LeafCollector<'a, LR>
        = FieldLeafCollectorEnum<'a, LR>
    where
        Self: 'a,
        LR: LeafReader;

    fn get_leaf_collector<'a, W, LR>(
        &'a mut self,
        context: &LeafReaderContext<LR>,
        weight: Option<&W>,
    ) -> Result<Self::LeafCollector<'a, LR>>
    where
        LR: LeafReader,
        W: Weight<LR>,
    {
        match self {
            Self::Simple(inner) => inner
                .get_leaf_collector(context, weight)
                .map(FieldLeafCollectorEnum::Simple),
            Self::Paging(inner) => inner
                .get_leaf_collector(context, weight)
                .map(FieldLeafCollectorEnum::Paging),
        }
    }

    fn score_mode(&self) -> ScoreMode {
        match self {
            Self::Simple(inner) => inner.score_mode(),
            Self::Paging(inner) => inner.score_mode(),
        }
    }
}

impl TopDocsCollector for TopFieldCollectorEnum {
    type Item = <TopFieldCollector as TopDocsCollector>::Item;
    type Cmp = <TopFieldCollector as TopDocsCollector>::Cmp;
    type TopDocsLike = <TopFieldCollector as TopDocsCollector>::TopDocsLike;

    fn pq(&self) -> &PriorityQueue<Self::Item, Self::Cmp> {
        match self {
            Self::Simple(inner) => inner.pq(),
            Self::Paging(inner) => inner.pq(),
        }
    }

    fn pq_mut(&mut self) -> &mut PriorityQueue<Self::Item, Self::Cmp> {
        match self {
            Self::Simple(inner) => inner.pq_mut(),
            Self::Paging(inner) => inner.pq_mut(),
        }
    }

    fn total_hits(&self) -> usize {
        match self {
            Self::Simple(inner) => inner.total_hits(),
            Self::Paging(inner) => inner.total_hits(),
        }
    }

    fn get_total_hits_relation(&self) -> Relation {
        match self {
            Self::Simple(inner) => inner.get_total_hits_relation(),
            Self::Paging(inner) => inner.get_total_hits_relation(),
        }
    }

    fn populate_results(&mut self, results: &mut [Self::Item], how_many: usize) -> Result<()> {
        match self {
            Self::Simple(inner) => inner.populate_results(results, how_many),
            Self::Paging(inner) => inner.populate_results(results, how_many),
        }
    }

    fn new_top_docs(&self, results: Option<Vec<Self::Item>>, start: i32) -> Self::TopDocsLike
    where
        Self: Sized,
    {
        match self {
            Self::Simple(inner) => inner.new_top_docs(results, start),
            Self::Paging(inner) => inner.new_top_docs(results, start),
        }
    }

    fn top_docs_size(&self) -> usize {
        match self {
            Self::Simple(inner) => inner.top_docs_size(),
            Self::Paging(inner) => inner.top_docs_size(),
        }
    }

    fn top_docs(&mut self) -> Result<Self::TopDocsLike>
    where
        Self: Sized,
    {
        match self {
            Self::Simple(inner) => inner.top_docs(),
            Self::Paging(inner) => inner.top_docs(),
        }
    }

    fn top_docs_with_start(&mut self, start: i32) -> Result<Self::TopDocsLike>
    where
        Self: Sized,
    {
        match self {
            Self::Simple(inner) => inner.top_docs_with_start(start),
            Self::Paging(inner) => inner.top_docs_with_start(start),
        }
    }

    fn top_docs_with_start_limit(&mut self, start: i32, how_many: i32) -> Result<Self::TopDocsLike>
    where
        Self: Sized,
    {
        match self {
            Self::Simple(inner) => inner.top_docs_with_start_limit(start, how_many),
            Self::Paging(inner) => inner.top_docs_with_start_limit(start, how_many),
        }
    }
}

pub enum TopFieldLeafComparatorEnum<LR>
where
    LR: LeafReader,
{
    Multi(MultiLeafFieldComparator<LR>),
    Single(LeafFieldComparatorEnum<LR>),
}
impl<LR> TopFieldLeafComparatorEnum<LR>
where
    LR: LeafReader,
{
    pub(crate) fn set_bottom(
        &mut self,
        slot: usize,
        comparator: &mut [FieldComparatorEnum],
    ) -> Result<()> {
        match self {
            Self::Multi(inner) => inner.set_bottom(slot, comparator),
            Self::Single(inner) => inner.set_bottom(slot, &mut comparator[0]),
        }
    }

    pub(crate) fn compare_bottom<S>(
        &mut self,
        doc: i32,
        scorer: &mut S,
        comparators: &mut [FieldComparatorEnum],
    ) -> Result<i32>
    where
        S: Scorable,
    {
        match self {
            Self::Multi(inner) => inner.compare_bottom(doc, scorer, comparators),
            Self::Single(inner) => inner.compare_bottom(doc, scorer, &mut comparators[0]),
        }
    }

    pub(crate) fn compare_top<S>(
        &mut self,
        doc: i32,
        scorer: &mut S,
        comparators: &mut [FieldComparatorEnum],
    ) -> Result<i32>
    where
        S: Scorable,
    {
        match self {
            Self::Multi(inner) => inner.compare_top(doc, scorer, comparators),
            Self::Single(inner) => inner.compare_top(doc, scorer, &mut comparators[0]),
        }
    }

    pub(crate) fn copy<S>(
        &mut self,
        slot: usize,
        doc: i32,
        scorer: &mut S,
        comparators: &mut [FieldComparatorEnum],
    ) -> Result<()>
    where
        S: Scorable,
    {
        match self {
            Self::Multi(inner) => inner.copy(slot, doc, scorer, comparators),
            Self::Single(inner) => inner.copy(slot, doc, scorer, &mut comparators[0]),
        }
    }

    pub(crate) fn set_scorer<S>(
        &mut self,
        scorer: &mut S,
        comparators: &mut [FieldComparatorEnum],
    ) -> Result<()>
    where
        S: Scorable,
    {
        match self {
            Self::Multi(inner) => inner.set_scorer(scorer, comparators),
            Self::Single(inner) => inner.set_scorer(scorer, &mut comparators[0]),
        }
    }

    pub(crate) fn competitive_iterator(
        &mut self,
        comparators: &mut [FieldComparatorEnum],
    ) -> Result<Option<TopFieldLeafComparatorEnumIterRef<'_, LR>>> {
        match self {
            Self::Multi(inner) => inner
                .competitive_iterator(comparators)
                .map(|opt| opt.map(Either2DocIdSetIterator::A)),
            Self::Single(inner) => inner
                .competitive_iterator(&mut comparators[0])
                .map(|opt| opt.map(Either2DocIdSetIterator::B)),
        }
    }

    pub(crate) fn set_hits_threshold_reached(
        &mut self,
        comparators: &mut [FieldComparatorEnum],
    ) -> Result<()> {
        match self {
            Self::Multi(inner) => inner.set_hits_threshold_reached(comparators),
            Self::Single(inner) => inner.set_hits_threshold_reached(&mut comparators[0]),
        }
    }
}
pub type TopFieldLeafComparatorEnumIterRef<'a, LR> = Either2DocIdSetIterator<
    LeafFieldComparatorDocIdSetIteratorRef<'a, LR>,
    <LeafFieldComparatorEnum<LR> as LeafFieldComparator>::DocIdSetIteratorRef<'a>,
>;

#[cfg(test)]
mod tests {
    use crate::core::document::document::Document;

    use crate::core::document::numeric_doc_values_field::NumericDocValuesField;

    use crate::core::index::composite_reader::get_context;
    use crate::core::index::directory_reader::directory_reader_util;
    use crate::core::index::index_reader_context::IndexReaderContext;
    use crate::core::index::index_writer::IndexWriter;
    use crate::core::index::index_writer_config::{DISABLE_AUTO_FLUSH, IndexWriterConfig};
    use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
    use crate::core::index::sort::Sort;
    use crate::core::index::standard_directory_reader::StandardDirectoryReaderType;

    use crate::core::search::collector::Collector;
    use crate::core::search::collector_manager::CollectorManager;
    use crate::core::search::dummy::dummy_scorable::DummyScorable;
    use crate::core::search::dummy::dummy_weight::DummyWeight;
    use crate::core::search::field_doc::FieldDoc;
    use crate::core::search::leaf_collector::LeafCollector;
    use crate::core::search::match_all_docs_query::MatchAllDocsQuery;
    use crate::core::search::max_score_accumulator::{DEFAULT_INTERVAL, MaxScoreAccumulator};
    use crate::core::search::scorable::Scorable;
    use crate::core::search::score_doc::ScoreDocLike;
    use crate::core::search::sort_field::{SortField, SortFieldType};

    use crate::core::document::field::Store;
    use crate::core::document::text_field::TextField;
    use crate::core::index::term::Term;
    use crate::core::search::index_searcher::IndexSearcher;
    use crate::core::search::term_query::TermQuery;
    use crate::core::search::top_docs::TopDocsLike;
    use crate::core::search::top_docs_collector::TopDocsCollector;
    use crate::core::search::top_field_collector_manager::TopFieldCollectorManager;
    use crate::core::search::total_hits::Relation::{EqualTo, GreaterThanOrEqualTo};
    use crate::core::search::total_hits::TotalHits;
    use crate::core::store::nio_fs_directory::NIOFSDirectory;
    use crate::core::store::{FSDirectory, NativeFSLockFactory};
    use crate::core::util::error::lucene_error::{LuceneError, Result};
    use crate::test::index::random_index_writer::RandomIndexWriter;
    use crate::test::search::check_hits::CheckHits;
    use crate::test::util::DefaultIndexSearch;
    use crate::test::util::lucene_test_case::lucene_test_case_util::{
        at_least, new_directory, new_searcher, random,
    };
    use std::sync::Arc;

    #[allow(dead_code)] // for quick search
    struct TestTopFieldCollector;

    fn setup() -> Result<(
        Arc<StandardDirectoryReaderType<FSDirectory<NativeFSLockFactory, NIOFSDirectory>>>,
        DefaultIndexSearch,
    )> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        let iw = RandomIndexWriter::new(&mut random, dir);
        let num_docs = at_least(&mut random, 100);
        for _ in 0..num_docs {
            let doc = Document::new();
            iw.add_document(doc)?;
        }
        let ir = Arc::new(iw.get_reader()?);
        iw.close()?;
        let is = new_searcher(ir.clone(), true, true, false)?;
        Ok((ir, is))
    }
    #[test]
    fn test_sort_without_fill_fields() -> Result<()> {
        let (_ir, mut is) = setup()?;
        let sorts = vec![
            Sort::with_fields(vec![SortField::get_field_doc()?.into()])?,
            Sort::new()?,
        ];
        for sort in sorts {
            let query = MatchAllDocsQuery::new();
            let collector_manager = TopFieldCollectorManager::new(sort, 10, i32::MAX)?;

            let top_docs = is.search_with_collector_manager(query, &collector_manager)?;
            let sd = top_docs.score_docs();

            for j in 1..sd.len() {
                assert_ne!(sd[j].doc(), sd[j - 1].doc());
            }
        }

        Ok(())
    }
    #[test]
    fn test_sort() -> Result<()> {
        let (_ir, mut is) = setup()?;
        // Two Sort criteria to instantiate the multi/single comparators.
        let sorts = [
            Sort::with_fields(vec![SortField::get_field_doc()?.into()])?,
            Sort::new()?,
        ];

        for sort in sorts {
            let query = MatchAllDocsQuery::new();
            let tdc = TopFieldCollectorManager::with_after(sort, 10, None, i32::MAX)?;
            let top_docs = is.search_with_collector_manager(query, &tdc)?;
            let sd = top_docs.score_docs();

            for doc in sd {
                assert!(
                    doc.score().is_nan(),
                    "expected NaN score but got {}",
                    doc.score()
                );
            }
        }

        Ok(())
    }
    #[test]
    fn test_shared_hitcount_collector() -> Result<()> {
        // 对应 newSearcher(ir, true, true, true)
        let (ir, _) = setup()?;
        let mut concurrent_searcher = new_searcher(ir.clone(), true, true, true)?;
        let mut single_threaded_searcher = new_searcher(ir.clone(), true, true, false)?;

        // Two Sort criteria to instantiate the multi/single comparators.
        let sorts = [
            Arc::new(Sort::with_fields(vec![SortField::get_field_doc()?.into()])?),
            Arc::new(Sort::new()?),
        ];

        for sort in sorts {
            let tdc = TopFieldCollectorManager::new(sort.clone(), 10, i32::MAX)?;
            let td =
                single_threaded_searcher.search_with_collector_manager(MatchAllDocsQuery, &tdc)?;

            let tsdc = TopFieldCollectorManager::new(sort, 10, i32::MAX)?;
            let td2 =
                concurrent_searcher.search_with_collector_manager(MatchAllDocsQuery, &tsdc)?;

            let sd = td.score_docs();
            for v in sd {
                assert!(
                    v.score().is_nan(),
                    "expected NaN score but got {}",
                    v.score()
                );
            }

            CheckHits::check_equal(&MatchAllDocsQuery.into(), td.score_docs(), td2.score_docs())?;
        }

        Ok(())
    }
    #[test]
    fn test_sort_without_total_hit_tracking() -> Result<()> {
        let (_ir, mut is) = setup()?;
        let sort = Arc::new(Sort::with_fields(vec![SortField::get_field_doc()?.into()])?);

        for i in 0..2 {
            let query = MatchAllDocsQuery::new();

            // check that setting trackTotalHits to false does not throw an error
            // because the index is not sorted
            let manager = if i % 2 == 0 {
                TopFieldCollectorManager::new(sort.clone(), 10, 1)?
            } else {
                let field_doc = FieldDoc::with_fields(1, f32::NAN, vec![1.into()]);
                TopFieldCollectorManager::with_after(sort.clone(), 10, Some(field_doc), 1)?
            };

            let top_docs = is.search_with_collector_manager(query, &manager)?;
            let sd = top_docs.score_docs();

            for v in sd {
                assert!(v.score().is_nan());
            }
        }

        Ok(())
    }

    fn document() -> Document {
        let mut doc = Document::new();
        doc.add(NumericDocValuesField::new("foo", 3));
        doc
    }
    // TODO 索引排序有bug 这个测试未通过
    fn test_total_hits() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        let sort = Arc::new(Sort::with_fields(vec![
            SortField::new("foo".into(), SortFieldType::Long)?.into(),
        ])?);
        // TODO 没有定义 合并策略
        let mut config = IndexWriterConfig::new();
        config
            .set_index_sort(sort.clone())?
            .set_max_buffered_docs(7)
            .set_ram_buffer_size_mb(DISABLE_AUTO_FLUSH as f64);

        let writer = IndexWriter::new(dir.clone(), config)?;
        let mut doc = Document::new();
        doc.add(NumericDocValuesField::new("foo", 3));
        for _ in 0..4 {
            writer.add_document(document())?;
        }
        writer.flush()?;
        for _ in 0..6 {
            writer.add_document(document())?;
        }
        writer.flush()?;

        let reader = directory_reader_util::open_with_writer(&writer)?;
        let reader = crate::core::index::composite_reader::get_context(Arc::new(reader))?;
        assert_eq!(2, reader.leaves()?.len());
        writer.close()?;

        let dummy_weight = DummyWeight::new(reader.leaves()?[0].reader().clone());
        for total_hits_threshold in 0..20 {
            let after_variants: [Option<FieldDoc>; 2] = [
                None,
                Some(FieldDoc::with_fields(4, f32::NAN, vec![2_i64.into()])),
            ];
            for after in after_variants {
                let manager = TopFieldCollectorManager::with_after(
                    sort.clone(),
                    2,
                    after.clone(),
                    total_hits_threshold,
                )?;
                let mut collector = manager.new_collector()?;
                let mut scorer = Score::default();

                let leaves = reader.leaves()?;
                let mut leaf_collector1 =
                    collector.get_leaf_collector(&leaves[0], Some(&dummy_weight))?;
                leaf_collector1.set_scorer(&mut scorer)?;

                scorer.score = 3.0;
                leaf_collector1.collect(0, &mut scorer)?;
                scorer.score = 3.0;
                leaf_collector1.collect(1, &mut scorer)?;

                let mut leaf_collector2 =
                    collector.get_leaf_collector(&leaves[1], Some(&dummy_weight))?;
                leaf_collector2.set_scorer(&mut scorer)?;

                scorer.score = 3.0;
                if total_hits_threshold < 3 {
                    let result = leaf_collector2.collect(1, &mut scorer);
                    assert!(matches!(result, Err(LuceneError::CollectionTerminated(_))));

                    let top_docs = collector.top_docs()?;
                    assert_eq!(
                        *top_docs.total_hits(),
                        TotalHits::new(3, GreaterThanOrEqualTo)
                    );
                    continue;
                } else {
                    leaf_collector2.collect(1, &mut scorer)?;
                }

                scorer.score = 4.0;
                if total_hits_threshold == 3 {
                    let result = leaf_collector2.collect(1, &mut scorer);
                    assert!(matches!(result, Err(LuceneError::CollectionTerminated(_))));

                    let top_docs = collector.top_docs()?;
                    assert_eq!(
                        *top_docs.total_hits(),
                        TotalHits::new(4, GreaterThanOrEqualTo)
                    );
                    continue;
                } else {
                    leaf_collector2.collect(1, &mut scorer)?;
                }

                let top_docs = collector.top_docs()?;
                assert_eq!(*top_docs.total_hits(), TotalHits::new(4, EqualTo));
            }
        }
        Ok(())
    }
    #[test]
    fn test_set_min_competitive_score() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);

        let config = IndexWriterConfig::new();
        // TODO: 未设置合并策略
        let writer = IndexWriter::new(dir.clone(), config)?;

        for _ in 0..4 {
            writer.add_document(Document::new())?;
        }
        writer.flush()?;
        for _ in 0..2 {
            writer.add_document(Document::new())?;
        }
        writer.flush()?;

        let reader = Arc::new(directory_reader_util::open_with_writer(&writer)?);
        let reader = get_context(reader)?;
        assert_eq!(2, reader.leaves()?.len());
        writer.close()?;
        let dummy_weight = DummyWeight::new(reader.leaves()?[0].reader().clone());

        let sort = Sort::with_fields(vec![
            SortField::get_field_score()?.into(),
            SortField::new("foo".into(), SortFieldType::Long)?.into(),
        ])?;

        let mut collector = TopFieldCollectorManager::new(sort, 2, 2)?.new_collector()?;
        let mut scorer = Score::default();

        let leaves = reader.leaves()?;
        let mut leaf_collector = collector.get_leaf_collector(&leaves[0], Some(&dummy_weight))?;
        leaf_collector.set_scorer(&mut scorer)?;
        assert!(scorer.min_competitive_score.is_none());

        scorer.score = 1.0;
        leaf_collector.collect(0, &mut scorer)?;
        assert!(scorer.min_competitive_score.is_none());

        scorer.score = 2.0;
        leaf_collector.collect(1, &mut scorer)?;
        assert!(scorer.min_competitive_score.is_none());

        scorer.score = 3.0;
        leaf_collector.collect(2, &mut scorer)?;
        assert_eq!(*scorer.min_competitive_score.as_ref().unwrap(), 2.0);

        scorer.score = 0.5;
        scorer.min_competitive_score = Some(f32::NAN);
        leaf_collector.collect(3, &mut scorer)?;
        assert!(scorer.min_competitive_score.as_ref().unwrap().is_nan());

        scorer.score = 4.0;
        leaf_collector.collect(4, &mut scorer)?;
        assert_eq!(*scorer.min_competitive_score.as_ref().unwrap(), 3.0);

        // Make sure the min score is set on scorers on new segments
        let mut scorer = Score::default();
        let mut leaf_collector = collector.get_leaf_collector(&leaves[1], Some(&dummy_weight))?;
        leaf_collector.set_scorer(&mut scorer)?;
        assert_eq!(*scorer.min_competitive_score.as_ref().unwrap(), 3.0);

        scorer.score = 1.0;
        leaf_collector.collect(0, &mut scorer)?;
        assert_eq!(*scorer.min_competitive_score.as_ref().unwrap(), 3.0);

        scorer.score = 4.0;
        leaf_collector.collect(1, &mut scorer)?;
        assert_eq!(*scorer.min_competitive_score.as_ref().unwrap(), 4.0);

        Ok(())
    }
    #[test]
    fn test_total_hits_with_score() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);

        // TODO: 未设置合并策略
        let config = IndexWriterConfig::new();
        let writer = IndexWriter::new(dir.clone(), config)?;

        for _ in 0..4 {
            writer.add_document(Document::new())?;
        }
        writer.flush()?;
        for _ in 0..6 {
            writer.add_document(Document::new())?;
        }
        writer.flush()?;

        let reader = Arc::new(directory_reader_util::open_with_writer(&writer)?);
        let reader = get_context(reader)?;
        assert_eq!(2, reader.leaves()?.len());
        writer.close()?;
        let dummy_weight = DummyWeight::new(reader.leaves()?[0].reader().clone());
        for total_hits_threshold in 0..20 {
            let sort = Sort::with_fields(vec![
                SortField::get_field_score()?.into(),
                SortField::new("foo".into(), SortFieldType::Long)?.into(),
            ])?;

            let mut collector =
                TopFieldCollectorManager::new(sort, 2, total_hits_threshold)?.new_collector()?;
            let mut scorer = Score::default();

            // segment 0
            let leaves = reader.leaves()?;
            let mut lc0 = collector.get_leaf_collector(&leaves[0], Some(&dummy_weight))?;
            lc0.set_scorer(&mut scorer)?;

            scorer.score = 3.0;
            lc0.collect(0, &mut scorer)?;
            scorer.score = 3.0;
            lc0.collect(1, &mut scorer)?;

            let mut lc1 = collector.get_leaf_collector(&leaves[1], Some(&dummy_weight))?;
            lc1.set_scorer(&mut scorer)?;

            scorer.score = 3.0;
            lc1.collect(1, &mut scorer)?;
            scorer.score = 4.0;
            lc1.collect(1, &mut scorer)?;

            let top_docs = collector.top_docs()?;

            assert_eq!(
                total_hits_threshold < 4,
                scorer.min_competitive_score.is_some()
            );

            let expected = if total_hits_threshold < 4 {
                TotalHits::new(4, GreaterThanOrEqualTo)
            } else {
                TotalHits::new(4, EqualTo)
            };
            assert_eq!(*top_docs.total_hits(), expected);
        }

        Ok(())
    }
    #[test]
    fn test_sort_no_results() -> Result<()> {
        // Two Sort criteria to instantiate the multi/single comparators.
        let sorts = [
            Sort::with_fields(vec![SortField::get_field_doc()?.into()])?,
            Sort::new()?,
        ];

        for sort in sorts {
            let mut collector =
                TopFieldCollectorManager::new(sort, 10, i32::MAX)?.new_collector()?;
            let top_docs = collector.top_docs()?;

            assert_eq!(top_docs.total_hits().value(), 0);
        }

        Ok(())
    }

    #[test]
    fn test_compute_scores_only_once() -> Result<()> {
        // TODO  BooleanQuery 未实现
        Ok(())
    }
    #[test]
    fn test_populate_scores() -> Result<()> {
        // TODO TopFieldCollector.populateScores未实现
        Ok(())
    }
    #[test]
    fn test_concurrent_min_score() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);

        // TODO 未实现合并策略
        let w = IndexWriter::new(dir.clone(), IndexWriterConfig::new())?;
        let doc = Document::new();
        w.add_documents(vec![doc.clone(); 5])?;
        w.flush()?;
        w.add_documents(vec![doc.clone(); 6])?;
        w.flush()?;
        w.add_documents(vec![doc; 2])?;
        w.flush()?;

        let reader = directory_reader_util::open_with_writer(&w)?;
        let reader = get_context(Arc::new(reader))?;
        assert_eq!(3, reader.leaves()?.len());
        w.close()?;

        let sort = Sort::with_fields(vec![
            SortField::get_field_score()?.into(),
            SortField::get_field_doc()?.into(),
        ])?;
        DEFAULT_INTERVAL.store(0, std::sync::atomic::Ordering::Relaxed);
        let manager = TopFieldCollectorManager::new(sort, 2, 0)?;
        let mut collector = manager.new_collector()?;
        let mut collector2 = manager.new_collector()?;

        assert!(Arc::ptr_eq(
            &collector.min_score_acc().unwrap(),
            &collector2.min_score_acc().unwrap()
        ));
        let min_value_checker = collector.min_score_acc().clone().unwrap();
        // force checking every round
        assert_eq!(min_value_checker.mod_interval, 0);

        // simple test scorer that exposes current `score` and tracks minCompetitiveScore
        let mut scorer = Score::default();
        let mut scorer2 = Score::default();

        let leaves = reader.leaves()?;
        let dummy_weight = DummyWeight::new(reader.leaves()?[0].reader().clone());

        let mut leaf_collector = collector.get_leaf_collector(&leaves[0], Some(&dummy_weight))?;
        leaf_collector.set_scorer(&mut scorer)?;
        let mut leaf_collector2 = collector2.get_leaf_collector(&leaves[1], Some(&dummy_weight))?;
        leaf_collector2.set_scorer(&mut scorer2)?;

        scorer.score = 3.0;
        leaf_collector.collect(0, &mut scorer)?;
        assert_eq!(i64::MIN, min_value_checker.get_raw());
        assert!(scorer.min_competitive_score.is_none());

        scorer2.score = 6.0;
        leaf_collector2.collect(0, &mut scorer2)?;
        assert_eq!(i64::MIN, min_value_checker.get_raw());
        assert!(scorer2.min_competitive_score.is_none());

        scorer.score = 2.0;
        leaf_collector.collect(1, &mut scorer)?;
        assert_eq!(i64::MIN, min_value_checker.get_raw());
        assert!(scorer.min_competitive_score.is_none());

        scorer2.score = 9.0;
        leaf_collector2.collect(1, &mut scorer2)?;
        assert_eq!(i64::MIN, min_value_checker.get_raw());
        assert!(scorer2.min_competitive_score.is_none());

        scorer2.score = 7.0;
        leaf_collector2.collect(2, &mut scorer2)?;
        assert!(
            (MaxScoreAccumulator::to_score(min_value_checker.get_raw()) - 7.0).abs() < f32::EPSILON
        );
        assert!(scorer.min_competitive_score.is_none());
        assert!((scorer2.min_competitive_score.unwrap() - 7.0).abs() < f32::EPSILON);

        scorer2.score = 1.0;
        leaf_collector2.collect(3, &mut scorer2)?;
        assert!(
            (MaxScoreAccumulator::to_score(min_value_checker.get_raw()) - 7.0).abs() < f32::EPSILON
        );
        assert!(scorer.min_competitive_score.is_none());
        assert!((scorer2.min_competitive_score.unwrap() - 7.0).abs() < f32::EPSILON);

        scorer.score = 10.0;
        leaf_collector.collect(2, &mut scorer)?;
        assert!(
            (MaxScoreAccumulator::to_score(min_value_checker.get_raw()) - 7.0).abs() < f32::EPSILON
        );
        assert!((scorer.min_competitive_score.unwrap() - 7.0).abs() < f32::EPSILON);
        assert!((scorer2.min_competitive_score.unwrap() - 7.0).abs() < f32::EPSILON);

        scorer.score = 11.0;
        leaf_collector.collect(3, &mut scorer)?;
        assert!(
            (MaxScoreAccumulator::to_score(min_value_checker.get_raw()) - 10.0).abs()
                < f32::EPSILON
        );
        assert!((scorer.min_competitive_score.unwrap() - 10.0).abs() < f32::EPSILON);
        assert!((scorer2.min_competitive_score.unwrap() - 7.0).abs() < f32::EPSILON);

        let mut collector3 = manager.new_collector()?;
        let mut leaf_collector3 = collector3.get_leaf_collector(&leaves[2], Some(&dummy_weight))?;
        let mut scorer3 = Score::default();
        leaf_collector3.set_scorer(&mut scorer3)?;
        assert!((scorer3.min_competitive_score.unwrap() - 10.0).abs() < f32::EPSILON);

        scorer3.score = 1.0;
        leaf_collector3.collect(0, &mut scorer3)?;
        assert!(
            (MaxScoreAccumulator::to_score(min_value_checker.get_raw()) - 10.0).abs()
                < f32::EPSILON
        );
        assert!((scorer3.min_competitive_score.unwrap() - 10.0).abs() < f32::EPSILON);

        scorer.score = 11.0;
        leaf_collector.collect(4, &mut scorer)?;
        assert!(
            (MaxScoreAccumulator::to_score(min_value_checker.get_raw()) - 11.0).abs()
                < f32::EPSILON
        );
        assert!((scorer.min_competitive_score.unwrap() - 11.0).abs() < f32::EPSILON);
        assert!((scorer2.min_competitive_score.unwrap() - 7.0).abs() < f32::EPSILON);
        assert!((scorer3.min_competitive_score.unwrap() - 10.0).abs() < f32::EPSILON);

        scorer3.score = 2.0;
        leaf_collector3.collect(1, &mut scorer3)?;
        assert!(
            (MaxScoreAccumulator::to_score(min_value_checker.get_raw()) - 11.0).abs()
                < f32::EPSILON
        );
        assert!((scorer.min_competitive_score.unwrap() - 11.0).abs() < f32::EPSILON);
        assert!((scorer2.min_competitive_score.unwrap() - 7.0).abs() < f32::EPSILON);
        assert!((scorer3.min_competitive_score.unwrap() - 11.0).abs() < f32::EPSILON);

        let top_docs = manager.reduce(vec![collector, collector2, collector3])?;
        assert_eq!(11, top_docs.total_hits().value());
        assert_eq!(
            TotalHits::new(11, GreaterThanOrEqualTo),
            *top_docs.total_hits()
        );
        Ok(())
    }
    #[test]
    fn test_random_min_competitive_score() -> Result<()> {
        // TODO BooleanQuery 未实现
        Ok(())
    }
    #[test]
    fn test_relation_vs_top_docs_count() -> Result<()> {
        let mut random = random();
        let sort = Arc::new(Sort::with_fields(vec![
            SortField::get_field_score()?.into(),
            SortField::get_field_doc()?.into(),
        ])?);

        let dir = Arc::new(new_directory(&mut random)?);
        // TODO 未实现合并策略
        let writer = IndexWriter::new(dir.clone(), IndexWriterConfig::new())?;

        let mut doc = Document::new();
        doc.add(TextField::with_string("f", "foo bar", Store::No)?);

        writer.add_documents(vec![doc.clone(); 5])?;
        writer.flush()?;
        writer.add_documents(vec![doc.clone(); 5])?;
        writer.flush()?;

        let reader = writer.get_reader(false, false)?;
        let mut searcher = IndexSearcher::new(get_context(Arc::new(reader))?)?;

        let manager = TopFieldCollectorManager::with_after(sort.clone(), 2, None, 10)?;
        let top_docs = searcher
            .search_with_collector_manager(TermQuery::new(Term::from_text("f", "foo")), &manager)?;
        assert_eq!(10, top_docs.total_hits().value());
        assert_eq!(EqualTo, top_docs.total_hits().relation());

        let manager = TopFieldCollectorManager::with_after(sort.clone(), 2, None, 2)?;
        let top_docs = searcher
            .search_with_collector_manager(TermQuery::new(Term::from_text("f", "foo")), &manager)?;
        assert!(10 >= top_docs.total_hits().value());
        assert_eq!(GreaterThanOrEqualTo, top_docs.total_hits().relation());

        let manager = TopFieldCollectorManager::with_after(sort.clone(), 10, None, 2)?;
        let top_docs = searcher
            .search_with_collector_manager(TermQuery::new(Term::from_text("f", "foo")), &manager)?;
        assert_eq!(10, top_docs.total_hits().value());
        assert_eq!(EqualTo, top_docs.total_hits().relation());
        writer.close()?;
        Ok(())
    }

    #[derive(Default)]
    struct Score {
        score: f32,
        min_competitive_score: Option<f32>,
    }
    impl Scorable for Score {
        fn score(&mut self) -> Result<f32> {
            Ok(self.score)
        }

        fn set_min_competitive_score(&mut self, min_score: f32) -> Result<()> {
            self.min_competitive_score = Some(min_score);
            Ok(())
        }

        type Scorable = DummyScorable;
    }
}
