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
use crate::core::index::impact::Impact;
use crate::core::index::impacts::Impacts;
use crate::core::index::impacts_enum::ImpactsEnum;
use crate::core::index::impacts_source::ImpactsSource;
use crate::core::search::approximation_disi::{ApproximationBitSetAccess, ApproximationDISI};
use crate::core::search::conjunction_disi::{ConjunctionDISI, ConjunctionDISIEnum};
use crate::core::search::doc_id_set_iterator::BitSetIteratorAccess;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::impacts_disi::SourceImpactsDISI;
use crate::core::search::max_score_cache::MaxScoreCache;
use crate::core::search::phrase_matcher::PhraseMatcher;
use crate::core::search::phrase_query::PostingsAndFreq;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::similarities_impl::similarities::SimScorer;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::priority_queue::{Compare, PriorityQueue};
pub type ImpactsApproximationType<IE, SS> = SourceImpactsDISI<ImpactsSourceImpl<IE>, SS>;
/// Expert: Find exact phrases
pub struct ExactPhraseMatcher<IE, SS, const TOP_SCORES: bool = false> {
  pub(crate) impacts_approximation: ImpactsApproximationType<IE, SS>,
  match_cost: f32,
  pub(crate) score_mode: ScoreMode,
  postings: Box<[PostingsAndPosition]>,
}
impl<IE: ImpactsEnum, SS: SimScorer, const TOP_SCORES: bool> ApproximationDISI
  for ExactPhraseMatcher<IE, SS, TOP_SCORES>
{
  fn approx_doc_id(&self) -> i32 {
    if TOP_SCORES {
      DocIdSetIterator::doc_id(&self.impacts_approximation)
    } else {
      DocIdSetIterator::doc_id(&self.impacts_approximation.iterator().impacts_enums)
    }
  }
  fn approx_next_doc(&mut self) -> Result<i32> {
    if TOP_SCORES {
      DocIdSetIterator::next_doc(&mut self.impacts_approximation)
    } else {
      DocIdSetIterator::next_doc(&mut self.impacts_approximation.iterator_mut().impacts_enums)
    }
  }
  fn approx_advance(&mut self, target: i32) -> Result<i32> {
    if TOP_SCORES {
      DocIdSetIterator::advance(&mut self.impacts_approximation, target)
    } else {
      DocIdSetIterator::advance(
        &mut self.impacts_approximation.iterator_mut().impacts_enums,
        target,
      )
    }
  }
  fn approx_slow_advance(&mut self, target: i32) -> Result<i32> {
    if TOP_SCORES {
      DocIdSetIterator::slow_advance(&mut self.impacts_approximation, target)
    } else {
      DocIdSetIterator::slow_advance(
        &mut self.impacts_approximation.iterator_mut().impacts_enums,
        target,
      )
    }
  }
  fn approx_cost(&self) -> Result<i64> {
    if TOP_SCORES {
      DocIdSetIterator::cost(&self.impacts_approximation)
    } else {
      DocIdSetIterator::cost(&self.impacts_approximation.iterator().impacts_enums)
    }
  }
}

impl<IE: ImpactsEnum, SS: SimScorer, const TOP_SCORES: bool> ApproximationBitSetAccess
  for ExactPhraseMatcher<IE, SS, TOP_SCORES>
{
  fn approx_is_bit_iter(&self) -> bool {
    if TOP_SCORES {
      BitSetIteratorAccess::is_bit_iter(&self.impacts_approximation)
    } else {
      BitSetIteratorAccess::is_bit_iter(&self.impacts_approximation.iterator().impacts_enums)
    }
  }
  fn approx_get(&self, index: usize) -> Result<bool> {
    if TOP_SCORES {
      BitSetIteratorAccess::get(&self.impacts_approximation, index)
    } else {
      BitSetIteratorAccess::get(&self.impacts_approximation.iterator().impacts_enums, index)
    }
  }
  fn approx_set_doc_id(&mut self, doc: i32) -> Result<()> {
    if TOP_SCORES {
      BitSetIteratorAccess::set_doc_id(&mut self.impacts_approximation, doc)
    } else {
      BitSetIteratorAccess::set_doc_id(
        &mut self.impacts_approximation.iterator_mut().impacts_enums,
        doc,
      )
    }
  }
  fn approx_bit_set_length(&self) -> Result<usize> {
    if TOP_SCORES {
      BitSetIteratorAccess::bit_set_length(&self.impacts_approximation)
    } else {
      BitSetIteratorAccess::bit_set_length(&self.impacts_approximation.iterator().impacts_enums)
    }
  }
}

impl<IE, SS, const TOP_SCORES: bool> ExactPhraseMatcher<IE, SS, TOP_SCORES>
where
  IE: ImpactsEnum,
  SS: SimScorer,
{
  pub fn new(
    postings: Vec<PostingsAndFreq<'_, IE>>,
    score_mode: ScoreMode,
    scorer: SS,
    match_cost: f32,
  ) -> Result<Self> {
    let postings_len = postings.len();
    let mut impacts_enum = Vec::with_capacity(postings_len);
    let mut postings_and_positions = Vec::with_capacity(postings_len);
    for (i, p) in postings.into_iter().enumerate() {
      impacts_enum.push(p.postings);
      postings_and_positions.push(PostingsAndPosition::new(i, p.position))
    }
    let wrapped_impacts_enum = ConjunctionDISI::create_conjunction(impacts_enum)?;
    let impacts_source = merge_impacts(wrapped_impacts_enum)?;
    let impacts_approximation =
      SourceImpactsDISI::from_source(MaxScoreCache::new(impacts_source, scorer));
    Ok(Self {
      impacts_approximation,
      match_cost,
      score_mode,
      postings: postings_and_positions.into_boxed_slice(),
    })
  }
  /**
   * Advances the position iterator to the first position on or after `target`. Returns
   * `false` if it is exhausted before reaching `target`, and `true` otherwise.
   */
  #[inline(always)]
  fn advance_position(&mut self, idx: usize, target: i32) -> Result<bool> {
    let posting = &mut self.postings[idx];
    if posting.pos >= target {
      return Ok(true);
    }
    let mut upto = posting.upto;
    if upto == posting.freq {
      return Ok(false);
    }
    // The position state and its postings owner are in independent fields.
    let iterator = self
      .impacts_approximation
      .iterator_mut()
      .impacts_enums
      .iterator_at_mut(posting.postings_idx);
    loop {
      posting.pos = iterator.next_position()?;
      upto += 1;
      posting.upto = upto;
      if posting.pos >= target {
        return Ok(true);
      }
      if upto == posting.freq {
        return Ok(false);
      }
    }
  }

  #[inline]
  fn posting(&self, idx: usize) -> &IE {
    self
      .impacts_approximation
      .iterator()
      .impacts_enums
      .iterator_at(idx)
  }
  #[inline]
  fn posting_mut(&mut self, idx: usize) -> &mut IE {
    self
      .impacts_approximation
      .iterator_mut()
      .impacts_enums
      .iterator_at_mut(idx)
  }
  pub(crate) fn approximation_top_scorers_mut(&mut self) -> &mut ImpactsApproximationType<IE, SS> {
    &mut self.impacts_approximation
  }
  pub(crate) fn approximation_mut(&mut self) -> &mut ConjunctionDISIEnum<IE> {
    &mut self.impacts_approximation.iterator_mut().impacts_enums
  }
}
impl<IE, SS, const TOP_SCORES: bool> PhraseMatcher for ExactPhraseMatcher<IE, SS, TOP_SCORES>
where
  IE: ImpactsEnum,
  SS: SimScorer,
{
  fn max_freq(&mut self) -> Result<f32> {
    let mut min_freq = self.postings[0].freq;

    for i in 1..self.postings.len() {
      min_freq = min_freq.min(self.postings[i].freq);
    }
    Ok(min_freq as f32)
  }

  fn reset(&mut self) -> Result<()> {
    let impacts_enums = &mut self.impacts_approximation.iterator_mut().impacts_enums;
    for posting in &mut self.postings {
      posting.freq = impacts_enums.iterator_at_mut(posting.postings_idx).freq()?;
      posting.pos = -1;
      posting.upto = 0;
    }

    Ok(())
  }

  fn next_match(&mut self) -> Result<bool> {
    if self.postings[0].upto < self.postings[0].freq {
      let postings_idx = self.postings[0].postings_idx;
      self.postings[0].pos = self.posting_mut(postings_idx).next_position()?;
      self.postings[0].upto += 1;
    } else {
      return Ok(false);
    }

    'advance_head: loop {
      let phrase_pos = self.postings[0].pos - self.postings[0].offset;
      for j in 1..self.postings.len() {
        let expected_pos = phrase_pos + self.postings[j].offset;
        // advance up to the same position as the lead
        if !self.advance_position(j, expected_pos)? {
          break 'advance_head;
        }

        if self.postings[j].pos != expected_pos {
          // we advanced too far
          let target = self.postings[j].pos - self.postings[j].offset + self.postings[0].offset;

          if self.advance_position(0, target)? {
            continue 'advance_head;
          } else {
            break 'advance_head;
          }
        }
      }
      return Ok(true);
    }
    Ok(false)
  }

  fn sloppy_weight(&self) -> f32 {
    1f32
  }

  fn start_position(&self) -> i32 {
    self.postings[0].pos
  }

  fn end_position(&self) -> i32 {
    self.postings[self.postings.len() - 1].pos
  }

  fn start_offset(&self) -> Result<i32> {
    let idx = self.postings[0].postings_idx;
    let posting = self.posting(idx);
    posting.start_offset()
  }

  fn end_offset(&self) -> Result<i32> {
    let idx = self.postings[self.postings.len() - 1].postings_idx;
    self.posting(idx).end_offset()
  }

  fn get_match_cost(&self) -> f32 {
    self.match_cost
  }
}
#[cfg(test)]
pub(crate) fn merge_impacts_from_ie<IE>(
  wrapped_impacts_enums: Vec<IE>,
) -> Result<ImpactsSourceImpl<IE>>
where
  IE: ImpactsEnum,
{
  merge_impacts(ConjunctionDISIEnum::A(ConjunctionDISI::new(
    wrapped_impacts_enums,
  )?))
}

/// Merge impacts for multiple terms of an exact phrase.
pub(crate) fn merge_impacts<IE>(
  wrapped_impacts_enums: ConjunctionDISIEnum<IE>,
) -> Result<ImpactsSourceImpl<IE>>
where
  IE: ImpactsEnum,
{
  // Iteration of block boundaries uses the impacts enum with the lower cost.
  // This is consistent with BlockMaxConjunctionScorer.
  let impacts_enums = &wrapped_impacts_enums;
  let mut tmp_lead_index: Option<usize> = None;
  for i in 0..impacts_enums.len() {
    let replace_lead = match tmp_lead_index {
      None => true,
      Some(lead_index) => {
        impacts_enums.iterator_at(i).cost()? < impacts_enums.iterator_at(lead_index).cost()?
      },
    };
    if replace_lead {
      tmp_lead_index = Some(i);
    }
  }
  let lead_index = expect_invariant!(
    tmp_lead_index,
    "a constructed conjunction contains at least one iterator to lead impacts merging"
  );
  Ok(ImpactsSourceImpl::new(wrapped_impacts_enums, lead_index))
}

pub struct ImpactsSourceImpl<IE> {
  pub(crate) impacts_enums: ConjunctionDISIEnum<IE>,
  lead_index: usize,
}
impl<IE> ImpactsSourceImpl<IE> {
  pub(crate) fn new(impacts_enums: ConjunctionDISIEnum<IE>, lead_index: usize) -> Self {
    Self {
      impacts_enums,
      lead_index,
    }
  }
}

impl<IE> ImpactsSource for ImpactsSourceImpl<IE>
where
  IE: ImpactsEnum,
{
  fn advance_shallow(&mut self, target: i32) -> Result<()> {
    for idx in 0..self.impacts_enums.len() {
      let impacts_enum = self.impacts_enums.iterator_at_mut(idx);
      impacts_enum.advance_shallow(target)?;
    }
    Ok(())
  }

  type Impacts<'a>
    = ImpactsImpl<IE::Impacts<'a>>
  where
    Self: 'a;

  fn get_impacts(&self) -> Result<Self::Impacts<'_>> {
    let mut impacts = Vec::with_capacity(self.impacts_enums.len());
    for idx in 0..self.impacts_enums.len() {
      let v = self.impacts_enums.iterator_at(idx);
      impacts.push(v.get_impacts()?);
    }
    Ok(ImpactsImpl::new(impacts, self.lead_index))
  }
}

impl<IE> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for ImpactsSourceImpl<IE>
where
  IE: ImpactsEnum,
{
}
impl<IE> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for ImpactsSourceImpl<IE> where
  IE: ImpactsEnum
{
}

impl<IE> DocIdSetIterator for ImpactsSourceImpl<IE>
where
  IE: ImpactsEnum,
{
  fn doc_id(&self) -> i32 {
    self.impacts_enums.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.impacts_enums.next_doc()
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.impacts_enums.advance(target)
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    self.impacts_enums.slow_advance(target)
  }

  fn cost(&self) -> Result<i64> {
    self.impacts_enums.cost()
  }
}

pub struct ImpactsImpl<I> {
  impacts: Vec<I>,
  lead_index: usize,
}

impl<I> ImpactsImpl<I> {
  fn new(impacts: Vec<I>, lead_index: usize) -> Self {
    Self {
      impacts,
      lead_index,
    }
  }
}
fn get_level<I>(impacts: &I, doc_id_up_to: i32) -> Option<usize>
where
  I: Impacts,
{
  let num_levels = impacts.num_levels();
  (0..num_levels).find(|&level| impacts.get_doc_id_upto(level) >= doc_id_up_to)
}

impl<I> Impacts for ImpactsImpl<I>
where
  I: Impacts,
{
  fn num_levels(&self) -> usize {
    // Delegate to the lead
    self.impacts[self.lead_index].num_levels()
  }

  fn get_doc_id_upto(&self, level: usize) -> i32 {
    // Delegate to the lead
    self.impacts[self.lead_index].get_doc_id_upto(level)
  }

  fn get_impacts(&'_ self, level: usize) -> Result<Vec<Impact>> {
    let doc_id_up_to = self.get_doc_id_upto(level);
    let impact_len = self.impacts.len();
    let mut sub_iterators = Vec::new();
    let mut first_sub = None;

    for i in 0..impact_len {
      let Some(impacts_level) = get_level(&self.impacts[i], doc_id_up_to) else {
        // This instance doesn't have useful impacts, ignore it: this is safe.
        continue;
      };

      let impact_list = self.impacts[i].get_impacts(impacts_level)?;
      let first = impact_list
        .first()
        .ok_or_else(|| LuceneError::illegal_state("impact is None"))?;

      if first.freq == i32::MAX && first.norm == 1 {
        // Dummy impacts, ignore it too.
        continue;
      }

      let sub = SubIterator::new(impact_list);
      if sub_iterators.is_empty() {
        if let Some(first) = first_sub.take() {
          // Keep a single useful list on the stack until a merge is needed.
          sub_iterators.reserve_exact(impact_len);
          sub_iterators.push(first);
          sub_iterators.push(sub);
        } else {
          first_sub = Some(sub);
        }
      } else {
        sub_iterators.push(sub);
      }
    }

    if let Some(sub) = first_sub {
      return Ok(sub.iterator);
    }
    if sub_iterators.is_empty() {
      return Ok(vec![Impact::new(i32::MAX, 1)]);
    }
    // Idea: merge impacts by freq. The tricky thing is that we need to
    // consider freq values that are not in the impacts too. For
    // instance if the list of impacts is [{freq=2,norm=10}, {freq=4,norm=12}],
    // there might well be a document that has a freq of 2 and a length of 11,
    // which was just not added to the list of impacts because {freq=2,norm=10}
    // is more competitive.
    // We walk impacts in parallel through a PQ ordered by freq. At any time,
    // the competitive impact consists of the lowest freq among all entries of
    // the PQ (the top) and the highest norm (tracked separately).
    // The empty and single-list cases above do not need a merge queue.
    let mut pq = PriorityQueue::new(impact_len, SubIteratorCmp)?;
    pq.add_all(sub_iterators)?;

    let mut merged_impacts: Vec<Impact> = Vec::new();

    let mut current_freq = {
      let top = pq
        .top()
        .ok_or_else(|| LuceneError::illegal_state("top is None"))?;
      top.current()?.freq
    };

    let mut current_norm = 0;
    for it in pq.iter_ref() {
      let norm = it.current()?.norm;
      if norm as u64 > current_norm as u64 {
        current_norm = norm;
      }
    }
    let mut top = pq
      .top_mut()
      .ok_or_else(|| LuceneError::illegal_state("top is None"))?;

    'outer: loop {
      if let Some(last) = merged_impacts.last_mut() {
        if last.norm == current_norm {
          last.freq = current_freq;
        } else {
          merged_impacts.push(Impact::new(current_freq, current_norm));
        }
      } else {
        merged_impacts.push(Impact::new(current_freq, current_norm));
      }

      loop {
        if !top.next() {
          // At least one clause doesn't have any more documents below the current norm,
          // so we can safely ignore further clauses. The only reason why they have more
          // impacts is because they cover more documents that we are not interested in.
          break 'outer;
        }

        let top_current = top.current()?;
        if top_current.norm as u64 > current_norm as u64 {
          current_norm = top_current.norm;
        }
        top = pq.update_top()?;

        let impact = top.current()?;
        if impact.freq != current_freq {
          break;
        }
      }

      current_freq = top.current()?.freq;
    }

    Ok(merged_impacts)
  }
}

struct SubIterator {
  iterator: Vec<Impact>,
  current: Option<usize>,
}
impl SubIterator {
  fn new(impacts: Vec<Impact>) -> Self {
    let current = if impacts.is_empty() { None } else { Some(0) };

    Self {
      iterator: impacts,
      current,
    }
  }
  fn next(&mut self) -> bool {
    match self.current {
      None => false,
      Some(idx) => {
        let next_idx = idx + 1;
        if next_idx >= self.iterator.len() {
          self.current = None;
          false
        } else {
          self.current = Some(next_idx);
          true
        }
      },
    }
  }
  fn current(&self) -> Result<&Impact> {
    match self.current {
      Some(idx) => Ok(&self.iterator[idx]),
      None => Err(LuceneError::illegal_state("current is None")),
    }
  }
}
struct SubIteratorCmp;
impl Compare<SubIterator> for SubIteratorCmp {
  fn less_than(&self, a: &SubIterator, b: &SubIterator) -> Result<bool> {
    match (a.current, b.current) {
      (Some(i1), Some(i2)) => Ok(a.iterator[i1].freq < b.iterator[i2].freq),
      _ => Err(LuceneError::illegal_state(
        "one of the iterators is exhausted",
      )),
    }
  }
}
struct PostingsAndPosition {
  postings_idx: usize,
  offset: i32,
  freq: i32,
  upto: i32,
  pos: i32,
}

impl PostingsAndPosition {
  fn new(postings_idx: usize, offset: i32) -> Self {
    Self {
      postings_idx,
      offset,
      freq: 0,
      upto: 0,
      pos: 0,
    }
  }
}
