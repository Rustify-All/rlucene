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
use crate::core::index::index_reader_context::{IRCLeafReader, IndexReaderContext};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::reader_util::ReaderUtil;
use crate::core::search::collector::Collector;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_stream::DocIdStream;
use crate::core::search::dummy::dummy_leaf_collector::DummyLeafCollector;
use crate::core::search::field_comparator::{
  FieldComparator, FieldComparatorEnum, FieldComparatorValue,
};
use crate::core::search::field_doc::FieldDoc;
use crate::core::search::field_value_hit_queue::{
  Entry, FieldValueHitQueueComparator, TopFieldScoreDoc,
};
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::leaf_collector::{LeafCollector, LeafCollectorEnum2};
use crate::core::search::leaf_field_comparator::LeafFieldComparatorDocIdSetIteratorRef;
use crate::core::search::max_score_accumulator::MaxScoreAccumulator;
use crate::core::search::multi_leaf_field_comparator::MultiLeafFieldComparator;
use crate::core::search::query::IntoQuery;
use crate::core::search::scorable::Scorable;
use crate::core::search::score_caching_wrapping_scorer::ScoreCachingWrappingLeafCollector;
use crate::core::search::score_doc::{ScoreDoc, ScoreDocLike};
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::search::sort::Sort;
use crate::core::search::sort_field::SortField;
use crate::core::search::sort_field_enum::SortFieldEnum;
use crate::core::search::top_docs_collector::{TopDocsCollector, TopDocsCollectorBase};
use crate::core::search::top_field_docs::TopFieldDocs;
use crate::core::search::total_hits::{Relation, TotalHits};
use crate::core::search::weight::Weight;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::priority_queue::PriorityQueue;
use crate::core::util::{CoreHelper, TryIntoInt};
use smallvec::SmallVec;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::vec;

/// A [`Collector`] that sorts by [`SortField`] using [`FieldComparator`]s.
///
/// See [`TopFieldCollectorManager`](crate::core::search::top_field_collector_manager::TopFieldCollectorManager) for creating a
/// [`TopFieldCollectorManager`](crate::core::search::top_field_collector_manager::TopFieldCollectorManager) with support for concurrency in [`IndexSearcher`].
pub struct TopFieldCollector {
  base: TopDocsCollectorBase<Entry, FieldValueHitQueueComparator>,
  num_hits: usize,
  total_hits_threshold: usize,
  can_set_min_score: bool,
  search_sort_part_of_index_sort: Option<bool>,
  min_score_acc: Option<Arc<MaxScoreAccumulator>>,
  min_competitive_score: f32,
  #[allow(dead_code)]
  num_comparators: usize,
  queue_full: bool,
  doc_base: i32,
  needs_scores: bool,
  score_mode: ScoreMode,
}
impl TopFieldCollector {
  pub fn new(
    pq: PriorityQueue<Entry, FieldValueHitQueueComparator>,
    num_hits: usize,
    total_hits_threshold: usize,
    needs_scores: bool,
    min_score_acc: Option<Arc<MaxScoreAccumulator>>,
  ) -> Result<Self> {
    let total_hits_threshold = std::cmp::max(total_hits_threshold, num_hits);
    let num_comparators = pq.get_comparators().len();

    let first_comparator = &pq.get_comparators()[0];
    let reverse_mul = pq.get_reverse_mul()[0];

    let (score_mode, can_set_min_score) = if matches!(first_comparator, FieldComparatorEnum::Relevance(_))
                && reverse_mul == 1// if the natural sort is preserved (sort by descending relevance)
                && total_hits_threshold != i32::MAX as usize
    {
      (ScoreMode::TopScores, true)
    } else {
      let can_set_min_score = false;
      let score_mode = if total_hits_threshold != i32::MAX as usize {
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

  fn increment_total_hits(&mut self) -> usize {
    self.base.total_hits += 1;
    debug_assert!(self.base.total_hits <= i32::MAX as usize);
    self.base.total_hits
  }

  fn comparators_mut(&mut self) -> &mut [FieldComparatorEnum] {
    self.pq_mut().get_comparators_mut()
  }

  fn total_hits_relation(&self) -> Relation {
    self.base.total_hits_relation
  }

  fn set_total_hits_relation(&mut self, relation: Relation) {
    self.base.total_hits_relation = relation;
  }

  pub(crate) fn update_global_min_competitive_score<S>(&mut self, scorer: &mut S) -> Result<()>
  where
    S: Scorable + ?Sized,
  {
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

  pub(crate) fn update_min_competitive_score<S>(&mut self, scorer: &mut S) -> Result<()>
  where
    S: Scorable + ?Sized,
  {
    if self.can_set_min_score && self.queue_full && self.base.total_hits > self.total_hits_threshold
    {
      let bottom = self.bottom()?;

      let first_comparator = &self.base.pq.get_comparators()[0];
      let min_score = first_comparator
        .value(bottom.slot)
        .and_then(FieldComparatorValue::into_f32)
        .ok_or_else(|| LuceneError::illegal_state("first comparator is not a float"))?;

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
  pub(crate) fn add(&mut self, slot: usize, doc: i32) -> Result<()> {
    let global_doc = doc + self.doc_base;
    self.pq_mut().add(Entry::new(slot, global_doc))?;

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
    bottom.base.doc = global_doc;
    let pq = self.pq_mut();
    pq.update_top()?;
    Ok(())
  }
  #[inline]
  fn bottom(&self) -> Result<&Entry> {
    self
      .base
      .pq
      .top()
      .ok_or_else(|| LuceneError::illegal_state("priority queue bottom missing"))
  }
  #[inline]
  fn bottom_mut(&mut self) -> Result<&mut Entry> {
    self
      .base
      .pq
      .top_mut()
      .ok_or_else(|| LuceneError::illegal_state("priority queue bottom missing"))
  }
}
/// Populate [`ScoreDoc::score`] scores of the given `topDocs`.
///
/// # Parameters
///
/// - `top_docs`: the top docs to populate
/// - `searcher`: the index searcher that has been used to compute `topDocs`
/// - `query`: the query that has been used to compute `topDocs`
///
/// # Errors
///
/// Returns [`LuceneErrorKind::IllegalArgument`](crate::core::util::error::lucene_error::LuceneErrorKind::IllegalArgument) if there is evidence that `topDocs` were computed
/// against a different searcher or a different query.
pub fn populate_scores<IRC, T, S, SIM>(
  top_docs: &mut [S],
  searcher: &IndexSearcher<IRC, SIM>,
  query: T,
) -> Result<()>
where
  SIM: Similarity,
  IRC: IndexReaderContext,
  IndexSearcher<IRC, SIM>: Sync,
  T: IntoQuery,
  S: ScoreDocLike,
{
  let mut top_docs_idxs: SmallVec<[usize; 16]> = SmallVec::with_capacity(top_docs.len());
  top_docs_idxs.extend(0..top_docs.len());
  top_docs_idxs.sort_unstable_by_key(|idx| top_docs[*idx].doc());

  let rewritten = searcher.rewrite(query)?;
  let weight = searcher.create_weight(rewritten, ScoreMode::Complete, 1.0)?;

  let contexts = searcher.get_leaf_contexts()?;
  let mut current_context_idx: Option<usize> = None;
  let mut current_scorer = None;

  for idx in top_docs_idxs {
    let score_doc = &mut top_docs[idx];
    let doc = score_doc.doc();

    let need_new_context = match current_context_idx {
      Some(context_idx) => {
        let ctx = &contexts[context_idx];
        doc as usize >= ctx.doc_base + ctx.reader().max_doc()? as usize
      },
      None => true,
    };

    if need_new_context {
      let max_doc = searcher.get_index_reader().max_doc()?;
      CoreHelper::check_index(doc, max_doc)?;

      let new_context_index = ReaderUtil::sub_index_with_leaves(doc, contexts);
      current_context_idx = Some(new_context_index);

      let ctx = &contexts[new_context_index];
      let scorer_supplier = weight.scorer_supplier(ctx, searcher)?;
      let mut scorer_supplier = scorer_supplier.ok_or_else(|| {
        LuceneError::illegal_argument(format!("Doc id {} doesn't match the query", doc))
      })?;

      current_scorer = Some(scorer_supplier.get(1, ctx, searcher)?);
    }

    let context_idx = current_context_idx
      .ok_or_else(|| LuceneError::illegal_argument("current_context_idx is not initialized"))?;
    let ctx = &contexts[context_idx];

    let scorer = current_scorer
      .as_mut()
      .ok_or_else(|| LuceneError::illegal_argument("scorer not initialized"))?;

    let leaf_doc = (doc as usize).checked_sub(ctx.doc_base);
    let leaf_doc: i32 = leaf_doc
      .ok_or_else(|| LuceneError::illegal_argument("leaf_doc < 0"))?
      .try_convert()?;

    let advanced = scorer.advance(leaf_doc)?;
    if leaf_doc != advanced {
      return Err(LuceneError::illegal_argument(format!(
        "Doc id {} doesn't match the query",
        doc
      )));
    }

    score_doc.set_score(scorer.score()?);
  }
  Ok(())
}

impl Collector for TopFieldCollector {
  type LeafCollector<'a, IRC>
    = DummyLeafCollector
  where
    Self: 'a,
    IRC: IndexReaderContext + 'a;

  fn get_leaf_collector<'a, W, IRC, SIM>(
    &'a mut self,
    _context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _weight: Option<&W>,
    _searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Self::LeafCollector<'a, IRC>>
  where
    SIM: Similarity,
    IRC: IndexReaderContext,
    W: Weight<IRC, SIM> + ?Sized,
    IndexSearcher<IRC, SIM>: Sync,
  {
    Err(LuceneError::unreachable(
      "should call Simple/PagingFieldCollector instead",
    ))
  }

  fn score_mode(&self) -> ScoreMode {
    self.score_mode
  }
}

impl TopDocsCollector for TopFieldCollector {
  type Item = Entry;
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

  fn populate_results(&mut self, results: &mut [TopFieldScoreDoc], how_many: usize) -> Result<()> {
    let pq = &mut self.base.pq;
    for i in (0..how_many).rev() {
      let entry = pq.pop_unchecked()?;
      results[i] = pq.fill_fields(entry).into();
    }
    Ok(())
  }

  fn new_top_docs(&self, results: Option<Vec<TopFieldScoreDoc>>, _start: i32) -> Self::TopDocsLike
  where
    Self: Sized,
  {
    let result = results.unwrap_or_default();
    // The concrete Simple/Paging collector attaches its sort fields in its result callback.
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
  collected_all_competitive_hits: bool,
  comparator: MultiLeafFieldComparator<LR>,
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
    if base.search_sort_part_of_index_sort.is_none() {
      let metadata = context.reader().get_metadata()?;
      let can_early_terminate = can_early_terminate(sort, metadata.get_sort().as_deref())?;
      base.search_sort_part_of_index_sort = Some(can_early_terminate);

      if can_early_terminate {
        let first_comparator = &mut base.comparators_mut()[0];
        first_comparator.disable_skipping();
      }
    }

    let leaf_comparators = base.pq_mut().get_leaf_comparator(context)?;
    let comparator = MultiLeafFieldComparator::new(leaf_comparators, base.pq().get_reverse_mul())?;

    Ok(Self {
      base,
      collected_all_competitive_hits: false,
      comparator,
    })
  }

  fn is_queue_full(&self) -> bool {
    self.base.queue_full
  }

  fn total_hits(&self) -> usize {
    self.base.total_hits()
  }

  fn total_hits_relation(&self) -> Relation {
    self.base.total_hits_relation()
  }

  fn update_min_competitive_score<S>(&mut self, scorer: &mut S) -> Result<()>
  where
    S: Scorable + ?Sized,
  {
    self.base.update_min_competitive_score(scorer)
  }

  fn compare_top<S>(&mut self, doc: i32, scorer: &mut S) -> Result<i32>
  where
    S: Scorable + ?Sized,
  {
    let (comparators, reverse_mul) = self.base.pq_mut().get_comparators_and_reverse_mul();
    self
      .comparator
      .compare_top(doc, scorer, comparators, reverse_mul)
  }

  pub(crate) fn count_hit<S>(&mut self, scorer: &mut S, _doc: i32) -> Result<()>
  where
    S: Scorable + ?Sized,
  {
    let hit_count_so_far = self.base.increment_total_hits();

    if let Some(acc) = &self.base.min_score_acc
      && (hit_count_so_far & acc.mod_interval) == 0
    {
      self.base.update_global_min_competitive_score(scorer)?;
    }

    if !self.base.score_mode.is_exhaustive()
      && self.base.total_hits_relation() == Relation::EqualTo
      && hit_count_so_far > self.base.total_hits_threshold
    {
      let comparators = self.base.comparators_mut();
      self.comparator.set_hits_threshold_reached(comparators)?;
      self
        .base
        .set_total_hits_relation(Relation::GreaterThanOrEqualTo);
    }

    Ok(())
  }
  #[inline(always)]
  pub(crate) fn threshold_check<S>(&mut self, doc: i32, scorer: &mut S) -> Result<bool>
  where
    S: Scorable + ?Sized,
  {
    let cmp_check = if self.collected_all_competitive_hits {
      true
    } else {
      let (comparators, reverse_mul) = self.base.pq_mut().get_comparators_and_reverse_mul();
      let cmp = self
        .comparator
        .compare_bottom(doc, scorer, comparators, reverse_mul)?;
      cmp <= 0
    };

    if cmp_check {
      // since docs are visited in doc Id order, if compare is 0, it means
      // this document is larger than anything else in the queue, and
      // therefore not competitive.
      if self.base.search_sort_part_of_index_sort.unwrap_or(false) {
        if self.base.total_hits() > self.base.total_hits_threshold {
          self
            .base
            .set_total_hits_relation(Relation::GreaterThanOrEqualTo);
          return Err(LuceneError::collection_terminated(
            "collection terminated due to early termination threshold",
          ));
        } else {
          self.collected_all_competitive_hits = true;
        }
      } else if self.base.total_hits_relation() == Relation::EqualTo {
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
    S: Scorable + ?Sized,
  {
    {
      let bottom = self.bottom()?;
      self
        .comparator
        .copy(bottom.slot, doc, scorer, self.base.comparators_mut())?;
    }
    self.base.update_bottom(doc)?;
    let bottom = self.bottom()?;
    self
      .comparator
      .set_bottom(bottom.slot, self.base.comparators_mut())?;
    self.base.update_min_competitive_score(scorer)?;

    Ok(())
  }
  pub(crate) fn collect_any_hit<S>(
    &mut self,
    doc: i32,
    hits_collected: usize,
    scorer: &mut S,
  ) -> Result<()>
  where
    S: Scorable + ?Sized,
  {
    // Startup transient: queue hasn't gathered numHits yet
    let slot = hits_collected - 1;
    // Copy hit into queue
    self
      .comparator
      .copy(slot, doc, scorer, self.base.comparators_mut())?;
    self.base.add(slot, doc)?;
    if self.base.queue_full {
      let bottom = self.bottom()?;
      self
        .comparator
        .set_bottom(bottom.slot, self.base.comparators_mut())?;
      self.base.update_min_competitive_score(scorer)?;
    }
    Ok(())
  }
  #[inline]
  fn bottom(&self) -> Result<&Entry> {
    self.base.bottom()
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
  type CompetitiveIterator<'view>
    = LeafFieldComparatorDocIdSetIteratorRef<'view, LR>
  where
    Self: 'view;

  fn set_scorer<SC: Scorable + ?Sized>(&mut self, scorer: &mut SC) -> Result<()> {
    let comparators = self.base.comparators_mut();
    self.comparator.set_scorer(scorer, comparators)?;

    if self.base.min_score_acc.is_none() {
      self.base.update_min_competitive_score(scorer)?;
    } else {
      self.base.update_global_min_competitive_score(scorer)?;
    }

    Ok(())
  }

  fn collect<SC: Scorable + ?Sized>(&mut self, _doc: i32, _scorer: &mut SC) -> Result<()> {
    Err(LuceneError::unsupported_operation("should not here"))
  }

  fn collect_stream<DS: DocIdStream + ?Sized, SC: Scorable + ?Sized>(
    &mut self,
    _stream: &mut DS,
    _scorer: &mut SC,
  ) -> Result<()> {
    Err(LuceneError::unsupported_operation("should not here"))
  }

  fn competitive_iterator(&mut self) -> Result<Option<Self::CompetitiveIterator<'_>>> {
    let comparators = self.base.comparators_mut();
    self.comparator.competitive_iterator(comparators)
  }
}

pub(crate) fn can_early_terminate(search_sort: &Sort, index_sort: Option<&Sort>) -> Result<bool> {
  Ok(
    can_early_terminate_on_doc_id(search_sort)?
      || can_early_terminate_on_prefix(search_sort, index_sort)?,
  )
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
  pub fn min_score_acc(&self) -> Option<Arc<MaxScoreAccumulator>> {
    self.base.min_score_acc.clone()
  }

  pub fn new(
    sort: Arc<Sort>,
    queue: PriorityQueue<Entry, FieldValueHitQueueComparator>,
    num_hits: usize,
    total_hits_threshold: usize,
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
  type LeafCollector<'a, IRC>
    = SimpleLeafCollector<'a, IRCLeafReader<IRC>>
  where
    Self: 'a,
    IRC: IndexReaderContext + 'a;

  fn get_leaf_collector<'a, W, IRC, SIM>(
    &'a mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _weight: Option<&W>,
    _searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Self::LeafCollector<'a, IRC>>
  where
    SIM: Similarity,
    IRC: IndexReaderContext,
    W: Weight<IRC, SIM> + ?Sized,
    IndexSearcher<IRC, SIM>: Sync,
  {
    self.base.min_competitive_score = 0.0;
    self.base.doc_base = context.doc_base as i32;
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

  fn populate_results(&mut self, results: &mut [TopFieldScoreDoc], how_many: usize) -> Result<()> {
    self.base.populate_results(results, how_many)
  }

  fn new_top_docs(&self, results: Option<Vec<TopFieldScoreDoc>>, start: i32) -> Self::TopDocsLike
  where
    Self: Sized,
  {
    let mut docs = self.base.new_top_docs(results, start);
    docs.fields = self.sort.get_sort().to_vec();
    docs
  }

  fn top_docs_size(&self) -> usize {
    self.base.top_docs_size()
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
  type CompetitiveIterator<'view>
    = LeafFieldComparatorDocIdSetIteratorRef<'view, LR>
  where
    Self: 'view;

  fn set_scorer<SC: Scorable + ?Sized>(&mut self, scorer: &mut SC) -> Result<()> {
    self.base.set_scorer(scorer)
  }

  fn collect<SC: Scorable + ?Sized>(&mut self, doc: i32, scorer: &mut SC) -> Result<()> {
    self.base.count_hit(scorer, doc)?;
    if self.base.is_queue_full() {
      if self.base.threshold_check(doc, scorer)? {
        return Ok(());
      }
      self.base.collect_competitive_hit(doc, scorer)?;
    } else {
      let hits_collected = self.base.total_hits();
      self.base.collect_any_hit(doc, hits_collected, scorer)?;
    }
    Ok(())
  }

  fn competitive_iterator(&mut self) -> Result<Option<Self::CompetitiveIterator<'_>>> {
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
  collected_hits: usize,
  after: ScoreDoc,
}

impl PagingFieldCollector {
  pub fn min_score_acc(&self) -> Option<Arc<MaxScoreAccumulator>> {
    self.base.min_score_acc.clone()
  }

  pub fn new<A>(
    sort: Arc<Sort>,
    queue: PriorityQueue<Entry, FieldValueHitQueueComparator>,
    after: A,
    num_hits: usize,
    total_hits_threshold: usize,
    min_score_acc: Option<Arc<MaxScoreAccumulator>>,
  ) -> Result<Self>
  where
    A: Into<Arc<FieldDoc>>,
  {
    let after = after.into();
    let mut base = TopFieldCollector::new(
      queue,
      num_hits,
      total_hits_threshold,
      sort.needs_scores(),
      min_score_acc,
    )?;

    // set top values for comparators
    let comparators = base.comparators_mut();
    for (comp, field_index) in comparators.iter_mut().zip(0..after.fields.len()) {
      comp.set_top_value_from_after(&after, field_index)?;
    }

    Ok(Self {
      base,
      sort,
      collected_hits: 0,
      after: after.base.clone(),
    })
  }
}

impl Collector for PagingFieldCollector {
  type LeafCollector<'a, IRC>
    = PagingLeafCollector<'a, IRCLeafReader<IRC>>
  where
    Self: 'a,
    IRC: IndexReaderContext + 'a;

  fn get_leaf_collector<'a, W, IRC, SIM>(
    &'a mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _weight: Option<&W>,
    _searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Self::LeafCollector<'a, IRC>>
  where
    SIM: Similarity,
    IRC: IndexReaderContext,
    W: Weight<IRC, SIM> + ?Sized,
    IndexSearcher<IRC, SIM>: Sync,
  {
    self.base.min_competitive_score = 0.0;
    self.base.doc_base = context.doc_base as i32;
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

  fn populate_results(&mut self, results: &mut [TopFieldScoreDoc], how_many: usize) -> Result<()> {
    self.base.populate_results(results, how_many)
  }

  fn new_top_docs(&self, results: Option<Vec<TopFieldScoreDoc>>, start: i32) -> Self::TopDocsLike
  where
    Self: Sized,
  {
    let mut docs = self.base.new_top_docs(results, start);
    docs.fields = self.sort.get_sort().to_vec();
    docs
  }

  fn top_docs_size(&self) -> usize {
    self.base.top_docs_size()
  }
}

/// Leaf collector for paging-based top field collection.
pub struct PagingFieldLeafCollector<'a, LR>
where
  LR: LeafReader,
{
  base: TopFieldLeafCollector<'a, LR>,
  after_doc: i32,
  collected_hits: &'a mut usize,
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
    collected_hits: &'a mut usize,
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
  type CompetitiveIterator<'view>
    = LeafFieldComparatorDocIdSetIteratorRef<'view, LR>
  where
    Self: 'view;

  fn set_scorer<SC: Scorable + ?Sized>(&mut self, scorer: &mut SC) -> Result<()> {
    self.base.set_scorer(scorer)
  }

  fn collect<SC: Scorable + ?Sized>(&mut self, doc: i32, scorer: &mut SC) -> Result<()> {
    self.base.count_hit(scorer, doc)?;
    if self.base.is_queue_full() && self.base.threshold_check(doc, scorer)? {
      return Ok(());
    }

    let top_cmp = self.base.compare_top(doc, scorer)?;

    if top_cmp > 0 || (top_cmp == 0 && doc <= self.after_doc) {
      // already collected in previous page
      if self.base.total_hits_relation() == Relation::EqualTo {
        // check if totalHitsThreshold is reached and we can update competitive score
        // necessary to account for possible update to global min competitive score
        self.base.update_min_competitive_score(scorer)?;
      }
      return Ok(());
    }

    if self.base.is_queue_full() {
      self.base.collect_competitive_hit(doc, scorer)?;
    } else {
      *self.collected_hits += 1;
      self
        .base
        .collect_any_hit(doc, *self.collected_hits, scorer)?;
    }

    Ok(())
  }

  fn competitive_iterator(&mut self) -> Result<Option<Self::CompetitiveIterator<'_>>> {
    self.base.competitive_iterator()
  }

  fn finish(&mut self) -> Result<()> {
    self.base.finish()
  }
}

type SimpleLeafCollector<'a, LR> = LeafCollectorEnum2<
  SimpleFieldLeafCollector<'a, LR>,
  ScoreCachingWrappingLeafCollector<SimpleFieldLeafCollector<'a, LR>>,
>;

type PagingLeafCollector<'a, LR> = LeafCollectorEnum2<
  PagingFieldLeafCollector<'a, LR>,
  ScoreCachingWrappingLeafCollector<PagingFieldLeafCollector<'a, LR>>,
>;

use crate::core::search::scorer_supplier::ScorerSupplier;
