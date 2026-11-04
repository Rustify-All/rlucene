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
use crate::core::index::index_reader_context::{IRCLeafReader, IndexReaderContext};
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::search::collector::Collector;
use crate::core::search::doc_id_stream::DocIdStream;
use crate::core::search::filter_scorable::FilterScorable;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::leaf_collector::{LeafCollector, LeafCollectorEnum3};
use crate::core::search::scorable::{ChildScorable, FixedScore, Scorable};
use crate::core::search::score_caching_wrapping_scorer::ScoreCachingWrappingLeafCollector;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::search::weight::Weight;
use crate::core::util::error::lucene_error::{LuceneError, LuceneErrorKind, Result};
use std::fmt::{Display, Formatter};

/// A [`Collector`] which allows running a search with several [`Collector`]s.
///
/// This module offers a [`wrap`] function which accepts a list of collectors and wraps them with
/// [`MultiCollector`], while filtering out the `None` ones.
///
/// **Note:** When mixing collectors that want to skip low-scoring hits
/// ([`ScoreMode::TopScores`]) with ones that require seeing all hits, such as mixing a
/// `TopScoreDocCollector` and a [`TotalHitCountCollector`](crate::core::search::total_hit_count_collector::TotalHitCountCollector), it should be faster to run the query
/// twice, once for each collector, rather than using this wrapper on a single search.
pub struct MultiCollector<C> {
  cache_scores: bool,
  collectors: Vec<C>,
}

impl<C> MultiCollector<C>
where
  C: Collector,
{
  pub fn new<I>(collectors: I) -> Result<Self>
  where
    I: IntoIterator<Item = C>,
  {
    let collectors = collectors.into_iter().collect::<Vec<C>>();
    if collectors.is_empty() {
      return Err(LuceneError::illegal_argument(
        "At least 1 collector must not be None",
      ));
    }
    let num_needs_scores = collectors
      .iter()
      .filter(|collector| collector.score_mode().needs_scores())
      .count();
    Ok(Self {
      cache_scores: num_needs_scores >= 2,
      collectors,
    })
  }
}

impl<C> MultiCollector<C> {
  /// Provides access to the wrapped [`Collector`]s for advanced use-cases.
  pub fn get_collectors(&self) -> &[C] {
    &self.collectors
  }

  pub fn get_collectors_mut(&mut self) -> &mut [C] {
    &mut self.collectors
  }

  pub fn into_collectors(self) -> Vec<C> {
    self.collectors
  }
}

/// Wraps a list of [`Collector`]s with a [`MultiCollector`].
///
/// This method works as follows:
///
/// - Filters out the `None` collectors, so they are not used during search time.
/// - If the input contains 1 real collector, it is returned as [`OneOrMultiCollector::One`].
/// - Otherwise the method returns a [`OneOrMultiCollector::Multi`] which wraps the non-`None` ones.
///
/// # Errors
///
/// Returns an error if either 0 collectors were input, or all collectors are `None`.
pub fn wrap<C, I>(collectors: I) -> Result<OneOrMultiCollector<C>>
where
  C: Collector,
  I: IntoIterator<Item = Option<C>>,
{
  // For the user's convenience, we allow None collectors to be passed.
  // However, to improve performance, these None collectors are found
  // and dropped from the array we save for actual collection time.
  let mut collectors = collectors.into_iter().flatten();
  let first = collectors
    .next()
    .ok_or_else(|| LuceneError::illegal_argument("At least 1 collector must not be None"))?;
  let Some(second) = collectors.next() else {
    return Ok(OneOrMultiCollector::One(first));
  };
  let collectors = [first, second].into_iter().chain(collectors);
  Ok(OneOrMultiCollector::Multi(MultiCollector::new(collectors)?))
}

impl<C> Collector for MultiCollector<C>
where
  C: Collector,
{
  type LeafCollector<'a, IRC>
    = LeafCollectorEnum3<
    C::LeafCollector<'a, IRC>,
    MultiLeafCollector<C::LeafCollector<'a, IRC>>,
    ScoreCachingWrappingLeafCollector<MultiLeafCollector<C::LeafCollector<'a, IRC>>>,
  >
  where
    Self: 'a,
    IRC: IndexReaderContext + 'a;

  fn get_leaf_collector<'a, W, IRC, SIM>(
    &'a mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    weight: Option<&W>,
    searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Self::LeafCollector<'a, IRC>>
  where
    SIM: Similarity,
    IRC: IndexReaderContext,
    W: Weight<IRC, SIM> + ?Sized,
    IndexSearcher<IRC, SIM>: Sync,
  {
    let global_score_mode = self.score_mode();
    let mut leaf_collectors = Vec::with_capacity(self.collectors.len());
    let mut leaf_score_mode = None;
    for collector in self.collectors.iter_mut() {
      let collector_score_mode = collector.score_mode();

      match collector.get_leaf_collector(context, weight, searcher) {
        Ok(leaf_collector) => {
          match leaf_score_mode {
            None => leaf_score_mode = Some(collector_score_mode),
            Some(score_mode) if score_mode == collector_score_mode => {},
            Some(_) => leaf_score_mode = Some(ScoreMode::Complete),
          }
          leaf_collectors.push(leaf_collector);
        },
        Err(error) if matches!(error.kind(), LuceneErrorKind::CollectionTerminated(_)) => continue,
        Err(e) => return Err(e),
      }
    }
    if leaf_collectors.is_empty() {
      Err(LuceneError::collection_terminated(""))
    } else if leaf_collectors.len() == 1
      && (global_score_mode == ScoreMode::TopScores
        || leaf_score_mode != Some(ScoreMode::TopScores))
    {
      Ok(LeafCollectorEnum3::A(leaf_collectors.remove(0)))
    } else {
      let leaf_collector =
        MultiLeafCollector::new(leaf_collectors, global_score_mode == ScoreMode::TopScores);
      if self.cache_scores {
        Ok(LeafCollectorEnum3::C(
          ScoreCachingWrappingLeafCollector::new(leaf_collector),
        ))
      } else {
        Ok(LeafCollectorEnum3::B(leaf_collector))
      }
    }
  }

  fn score_mode(&self) -> ScoreMode {
    let mut score_mode = None;
    for collector in &self.collectors {
      let collector_score_mode = collector.score_mode();
      match score_mode {
        None => score_mode = Some(collector_score_mode),
        Some(current) if current == collector_score_mode => {},
        Some(current) => {
          score_mode = Some(
            if current.needs_scores() || collector_score_mode.needs_scores() {
              ScoreMode::Complete
            } else {
              ScoreMode::CompleteNoScores
            },
          );
        },
      }
    }
    score_mode.unwrap_or(ScoreMode::CompleteNoScores)
  }

  fn set_weight<W, IRC, SIM>(&self, weight: Option<&W>) -> Result<()>
  where
    SIM: Similarity,
    IRC: IndexReaderContext,
    W: Weight<IRC, SIM> + ?Sized,
  {
    for collector in self.collectors.iter() {
      collector.set_weight(weight)?;
    }
    Ok(())
  }
}

/// Result of [`wrap`].
pub enum OneOrMultiCollector<C> {
  One(C),
  Multi(MultiCollector<C>),
}

pub enum OneOrMultiLeafCollector<LC> {
  One(LC),
  MultiSingle(LC),
  Multi(MultiLeafCollector<LC>),
  MultiCached(ScoreCachingWrappingLeafCollector<MultiLeafCollector<LC>>),
}

impl<LC> Display for OneOrMultiLeafCollector<LC>
where
  LC: LeafCollector,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::One(collector) | Self::MultiSingle(collector) => Display::fmt(collector, f),
      Self::Multi(collector) => Display::fmt(collector, f),
      Self::MultiCached(collector) => Display::fmt(collector, f),
    }
  }
}

impl<LC> LeafCollector for OneOrMultiLeafCollector<LC>
where
  LC: LeafCollector,
{
  type CompetitiveIterator<'a>
    = crate::core::search::doc_id_set_iterator::DocIdSetIteratorEnum2<
    LC::CompetitiveIterator<'a>,
    crate::core::search::doc_id_set_iterator::EmptyDISI,
  >
  where
    Self: 'a;

  fn set_scorer<SC: Scorable + ?Sized>(&mut self, scorer: &mut SC) -> Result<()> {
    match self {
      Self::One(collector) | Self::MultiSingle(collector) => collector.set_scorer(scorer),
      Self::Multi(collector) => collector.set_scorer(scorer),
      Self::MultiCached(collector) => collector.set_scorer(scorer),
    }
  }

  fn collect<SC: Scorable + ?Sized>(&mut self, doc: i32, scorer: &mut SC) -> Result<()> {
    match self {
      Self::One(collector) | Self::MultiSingle(collector) => collector.collect(doc, scorer),
      Self::Multi(collector) => collector.collect(doc, scorer),
      Self::MultiCached(collector) => collector.collect(doc, scorer),
    }
  }

  fn collect_stream<DS: DocIdStream + ?Sized, SC: Scorable + ?Sized>(
    &mut self,
    stream: &mut DS,
    scorer: &mut SC,
  ) -> Result<()> {
    match self {
      Self::One(collector) | Self::MultiSingle(collector) => {
        collector.collect_stream(stream, scorer)
      },
      Self::Multi(collector) => collector.collect_stream(stream, scorer),
      Self::MultiCached(collector) => collector.collect_stream(stream, scorer),
    }
  }

  fn competitive_iterator(&mut self) -> Result<Option<Self::CompetitiveIterator<'_>>> {
    match self {
      Self::One(collector) | Self::MultiSingle(collector) => Ok(
        collector
          .competitive_iterator()?
          .map(crate::core::search::doc_id_set_iterator::DocIdSetIteratorEnum2::A),
      ),
      Self::Multi(collector) => Ok(
        collector
          .competitive_iterator()?
          .map(crate::core::search::doc_id_set_iterator::DocIdSetIteratorEnum2::B),
      ),
      Self::MultiCached(collector) => Ok(
        collector
          .competitive_iterator()?
          .map(crate::core::search::doc_id_set_iterator::DocIdSetIteratorEnum2::B),
      ),
    }
  }

  fn finish(&mut self) -> Result<()> {
    match self {
      Self::One(collector) | Self::MultiSingle(collector) => collector.finish(),
      Self::Multi(collector) => collector.finish(),
      Self::MultiCached(collector) => collector.finish(),
    }
  }
}

impl<C> Collector for OneOrMultiCollector<C>
where
  C: Collector,
{
  type LeafCollector<'a, IRC>
    = OneOrMultiLeafCollector<C::LeafCollector<'a, IRC>>
  where
    Self: 'a,
    IRC: IndexReaderContext + 'a;

  fn get_leaf_collector<'a, W, IRC, SIM>(
    &'a mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    weight: Option<&W>,
    searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Self::LeafCollector<'a, IRC>>
  where
    SIM: Similarity,
    IRC: IndexReaderContext,
    W: Weight<IRC, SIM> + ?Sized,
    IndexSearcher<IRC, SIM>: Sync,
  {
    match self {
      Self::One(collector) => collector
        .get_leaf_collector(context, weight, searcher)
        .map(OneOrMultiLeafCollector::One),
      Self::Multi(collector) => {
        let collector = collector.get_leaf_collector(context, weight, searcher)?;
        Ok(match collector {
          LeafCollectorEnum3::A(collector) => OneOrMultiLeafCollector::MultiSingle(collector),
          LeafCollectorEnum3::B(collector) => OneOrMultiLeafCollector::Multi(collector),
          LeafCollectorEnum3::C(collector) => OneOrMultiLeafCollector::MultiCached(collector),
        })
      },
    }
  }

  fn score_mode(&self) -> ScoreMode {
    match self {
      Self::One(collector) => collector.score_mode(),
      Self::Multi(collector) => collector.score_mode(),
    }
  }

  fn set_weight<W, IRC, SIM>(&self, weight: Option<&W>) -> Result<()>
  where
    SIM: Similarity,
    IRC: IndexReaderContext,
    W: Weight<IRC, SIM> + ?Sized,
  {
    match self {
      Self::One(collector) => collector.set_weight(weight),
      Self::Multi(collector) => collector.set_weight(weight),
    }
  }
}

pub struct MultiLeafCollector<LC> {
  collectors: Box<[Option<LC>]>,
  min_scores: Option<Box<[f32]>>,
  skip_non_competitive_scores: bool,
}

impl<LC> MultiLeafCollector<LC> {
  fn new(collectors: Vec<LC>, skip_non_competitive_scores: bool) -> Self {
    let min_scores = if skip_non_competitive_scores {
      Some(vec![0.0; collectors.len()].into_boxed_slice())
    } else {
      None
    };
    Self {
      collectors: collectors
        .into_iter()
        .map(Some)
        .collect::<Vec<_>>()
        .into_boxed_slice(),
      min_scores,
      skip_non_competitive_scores,
    }
  }

  fn all_collectors_terminated(&self) -> bool {
    self.collectors.iter().all(Option::is_none)
  }
}

impl<LC> Display for MultiLeafCollector<LC> {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}

impl<LC> LeafCollector for MultiLeafCollector<LC>
where
  LC: LeafCollector,
{
  type CompetitiveIterator<'a>
    = crate::core::search::doc_id_set_iterator::EmptyDISI
  where
    Self: 'a;

  fn set_scorer<SC: Scorable + ?Sized>(&mut self, scorer: &mut SC) -> Result<()> {
    if self.skip_non_competitive_scores {
      let min_scores = self.min_scores.as_mut().ok_or_else(|| {
        LuceneError::illegal_state("min scores exist when non-competitive scores are skipped")
      })?;
      for (idx, collector) in self.collectors.iter_mut().enumerate() {
        if let Some(c) = collector {
          let mut scorer = MinCompetitiveScoreAwareScorable::new(scorer, idx, min_scores);
          c.set_scorer(&mut scorer)?;
        }
      }
    } else {
      let mut scorer = FilterScorable::new(scorer);
      for collector in self.collectors.iter_mut().flatten() {
        collector.set_scorer(&mut scorer)?;
      }
    }
    Ok(())
  }

  // NOTE: not propagating collect(DocIdStream) since DocIdStreams may only be consumed once.
  fn collect<SC: Scorable + ?Sized>(&mut self, doc: i32, scorer: &mut SC) -> Result<()> {
    if self.skip_non_competitive_scores {
      let min_scores = self.min_scores.as_mut().ok_or_else(|| {
        LuceneError::illegal_state("min scores exist when non-competitive scores are skipped")
      })?;
      let collectors = &mut self.collectors;
      for idx in 0..collectors.len() {
        if let Some(collector) = collectors[idx].as_mut() {
          let mut scorer = MinCompetitiveScoreAwareScorable::new(scorer, idx, min_scores);
          match collector.collect(doc, &mut scorer) {
            Ok(()) => {},
            Err(error) if matches!(error.kind(), LuceneErrorKind::CollectionTerminated(_)) => {
              collector.finish()?;
              collectors[idx] = None;
              if collectors.iter().all(Option::is_none) {
                return Err(LuceneError::collection_terminated(""));
              }
            },
            Err(e) => return Err(e),
          }
        }
      }
    } else {
      let mut scorer = FilterScorable::new(scorer);
      for idx in 0..self.collectors.len() {
        if let Some(collector) = self.collectors[idx].as_mut() {
          match collector.collect(doc, &mut scorer) {
            Ok(()) => {},
            Err(error) if matches!(error.kind(), LuceneErrorKind::CollectionTerminated(_)) => {
              collector.finish()?;
              self.collectors[idx] = None;
              if self.all_collectors_terminated() {
                return Err(LuceneError::collection_terminated(""));
              }
            },
            Err(e) => return Err(e),
          }
        }
      }
    }
    Ok(())
  }

  fn finish(&mut self) -> Result<()> {
    for collector in self.collectors.iter_mut().flatten() {
      collector.finish()?;
    }
    Ok(())
  }
}

pub struct MinCompetitiveScoreAwareScorable<'a, S>
where
  S: ?Sized,
{
  in_: &'a mut S,
  idx: usize,
  min_scores: &'a mut [f32],
}

impl<'a, S> MinCompetitiveScoreAwareScorable<'a, S>
where
  S: ?Sized,
{
  pub fn new(in_: &'a mut S, idx: usize, min_scores: &'a mut [f32]) -> Self {
    Self {
      in_,
      idx,
      min_scores,
    }
  }

  fn min_score(&self) -> f32 {
    self
      .min_scores
      .iter()
      .copied()
      .fold(f32::MAX, |min, score| if score < min { score } else { min })
  }
}

impl<S> Scorable for MinCompetitiveScoreAwareScorable<'_, S>
where
  S: Scorable + ?Sized,
{
  type Child<'a>
    = S::Child<'a>
  where
    Self: 'a;

  fn score(&mut self) -> Result<f32> {
    self.in_.score()
  }

  fn smoothing_score(&mut self, doc_id: i32) -> Result<f32> {
    self.in_.smoothing_score(doc_id)
  }

  fn set_min_competitive_score(&mut self, min_score: f32) -> Result<()> {
    if min_score > self.min_scores[self.idx] {
      self.min_scores[self.idx] = min_score;
      self.in_.set_min_competitive_score(self.min_score())?;
    }
    Ok(())
  }

  fn get_children(&mut self) -> Result<Vec<ChildScorable<Self::Child<'_>>>> {
    self.in_.get_children()
  }

  fn cost(&self) -> Result<i64> {
    self.in_.cost()
  }

  #[cfg(test)]
  fn scorable_test_type_name(&self) -> &'static str {
    std::any::type_name::<Self>()
  }
}

impl<S> FixedScore for MinCompetitiveScoreAwareScorable<'_, S> where S: ?Sized {}
