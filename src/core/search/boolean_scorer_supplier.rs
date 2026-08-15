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
use crate::core::search::block_max_conjunction_bulk_scorer::BlockMaxConjunctionBulkScorer;
use crate::core::search::block_max_conjunction_scorer::BlockMaxConjunctionScorer;
use crate::core::search::boolean_clause::Occur;
use crate::core::search::boolean_scorer::BooleanScorer;
use crate::core::search::bulk_scorer::BulkScorer;
use crate::core::search::conjunction_bulk_scorer::ConjunctionBulkScorer;
use crate::core::search::conjunction_scorer::ConjunctionScorer;
use crate::core::search::constant_score_scorer::ConstantScoreScorer;
use crate::core::search::disjunction_scorer::DisjunctionScorer;
use crate::core::search::disjunction_sum_scorer::DisjunctionSumScorer;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::leaf_collector::LeafCollector;
use crate::core::search::max_score_bulk_scorer::MaxScoreBulkScorer;
use crate::core::search::query::{QueryWeightSs, QueryWeightSsBulkScorer, QueryWeightSsScorer};
use crate::core::search::req_excl_bulk_scorer::ReqExclBulkScorer;
use crate::core::search::req_excl_scorer::ReqExclScorer;
use crate::core::search::req_opt_sum_scorer::ReqOptSumScorer;
use crate::core::search::scorable::Scorable;
use crate::core::search::score::Score;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::score_mode::ScoreMode::CompleteNoScores;
use crate::core::search::scorer::{Scorer, ScorerEnum2, ScorerEnum3, TwoPhaseState};
use crate::core::search::scorer_supplier::ScorerSupplier;
use crate::core::search::scorer_util::ScorerUtil;
use crate::core::search::two_phase_iterator::TwoPhaseIterator;
use crate::core::search::wand_scorer::WANDScorer;
use crate::core::search::weight::DefaultBulkScorer;
use crate::core::util::TryIntoInt;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::{LuceneError, Result};
#[cfg(test)]
use std::any::Any;
use std::collections::HashMap;
use std::fmt::{Display, Formatter};

pub struct BooleanScorerSupplier<IRC> {
  pub(crate) subs: HashMap<Occur, Vec<QueryWeightSs<IRC>>>,
  score_mode: ScoreMode,
  min_should_match: i32,
  max_doc: i32,
  cost: i64,
  top_level_scoring_clause: bool,
}
impl<IRC> BooleanScorerSupplier<IRC>
where
  IRC: IndexReaderContext,
{
  pub(crate) fn new(
    subs: HashMap<Occur, Vec<QueryWeightSs<IRC>>>,
    score_mode: ScoreMode,
    min_should_match: i32,
    max_doc: i32,
  ) -> Result<Self> {
    if subs.len() != 4
      || !subs.contains_key(&Occur::Should)
      || !subs.contains_key(&Occur::Must)
      || !subs.contains_key(&Occur::Filter)
      || !subs.contains_key(&Occur::MustNot)
    {
      return Err(LuceneError::illegal_argument(
        "subs must contain exactly 4 keys: SHOULD, MUST, FILTER, MUST_NOT",
      ));
    }
    if min_should_match < 0 {
      return Err(LuceneError::illegal_argument(format!(
        "minShouldMatch must be positive, but got: {}",
        min_should_match
      )));
    }

    let should_len = subs.get(&Occur::Should).map(|v| v.len()).unwrap_or(0) as i32;
    let must_len = subs.get(&Occur::Must).map(|v| v.len()).unwrap_or(0) as i32;
    let filter_len = subs.get(&Occur::Filter).map(|v| v.len()).unwrap_or(0) as i32;

    if min_should_match != 0 && min_should_match >= should_len {
      return Err(LuceneError::illegal_argument(
        "minShouldMatch must be strictly less than the number of SHOULD clauses",
      ));
    }

    if !score_mode.needs_scores()
      && min_should_match == 0
      && should_len > 0
      && (must_len + filter_len) > 0
    {
      return Err(LuceneError::illegal_argument(
        "Cannot pass purely optional clauses if scores are not needed",
      ));
    }

    if should_len + must_len + filter_len == 0 {
      return Err(LuceneError::illegal_argument(
        "There should be at least one positive clause",
      ));
    }

    Ok(Self {
      subs,
      score_mode,
      min_should_match,
      max_doc,
      cost: -1,
      top_level_scoring_clause: false,
    })
  }
  fn compute_cost(
    &mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<i64> {
    let mut min_required_cost: Option<i64> = None;

    if let Some(v) = self.subs.get_mut(&Occur::Must) {
      for ss in v.iter_mut() {
        let c = ss.cost(context, searcher)?;
        min_required_cost = Some(match min_required_cost {
          Some(prev) => prev.min(c),
          None => c,
        });
      }
    }

    if let Some(v) = self.subs.get_mut(&Occur::Filter) {
      for ss in v.iter_mut() {
        let c = ss.cost(context, searcher)?;
        min_required_cost = Some(match min_required_cost {
          Some(prev) => prev.min(c),
          None => c,
        });
      }
    }

    if self.min_should_match == 0
      && let Some(c) = min_required_cost
    {
      return Ok(c);
    }
    let should_cost = match self.subs.get_mut(&Occur::Should) {
      Some(v) => {
        let mut costs = Vec::with_capacity(v.len());
        for ss in v.iter_mut() {
          costs.push(ss.cost(context, searcher)?);
        }

        ScorerUtil::cost_with_min_should_match(
          costs,
          v.len(),
          self.min_should_match.try_convert()?,
        )?
      },
      None => i64::MAX,
    };

    Ok(std::cmp::min(
      min_required_cost.unwrap_or(i64::MAX),
      should_cost,
    ))
  }
  fn get_internal(
    &mut self,
    mut lead_cost: i64,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<QueryWeightSsScorer> {
    // three cases: conjunction, disjunction, or mix
    lead_cost = std::cmp::min(lead_cost, self.cost(context, searcher)?);
    let should_empty = self
      .subs
      .get(&Occur::Should)
      .map(|v| v.is_empty())
      .unwrap_or(true);

    let filter_empty = self
      .subs
      .get(&Occur::Filter)
      .map(|v| v.is_empty())
      .unwrap_or(true);

    let must_empty = self
      .subs
      .get(&Occur::Must)
      .map(|v| v.is_empty())
      .unwrap_or(true);

    // pure conjunction
    if should_empty {
      let [filter_v, must_v] = self.subs.get_disjoint_mut([&Occur::Filter, &Occur::Must]);

      let filter_slice = filter_v.unwrap().as_mut_slice();
      let must_slice = must_v.unwrap().as_mut_slice();
      let req = Self::req(
        filter_slice,
        must_slice,
        lead_cost,
        self.top_level_scoring_clause,
        context,
        searcher,
        &self.score_mode,
      )?;
      let v = Self::excl(
        req,
        self
          .subs
          .get_mut(&Occur::MustNot)
          .map(|v| v.as_mut_slice())
          .unwrap_or(&mut []),
        lead_cost,
        context,
        searcher,
      )?;
      return Ok(v);
    }

    // pure disjunction
    if filter_empty && must_empty {
      let opt = Self::opt(
        self
          .subs
          .get_mut(&Occur::Should)
          .map(|v| v.as_mut_slice())
          .unwrap_or(&mut []),
        self.min_should_match,
        self.score_mode,
        lead_cost,
        self.top_level_scoring_clause,
        context,
        searcher,
      )?;
      let v = Self::excl(
        opt,
        self
          .subs
          .get_mut(&Occur::MustNot)
          .map(|v| v.as_mut_slice())
          .unwrap_or(&mut []),
        lead_cost,
        context,
        searcher,
      )?;
      return Ok(Box::new(v));
    }
    //
    // // conjunction-disjunction mix:
    // // we create the required and optional pieces, and then
    // // combine the two: if minNrShouldMatch > 0, then it's a conjunction: because the
    // // optional side must match. otherwise it's required + optional
    let [should_v, must_v, filter_v, must_not_v] = self.subs.get_disjoint_mut([
      &Occur::Should,
      &Occur::Must,
      &Occur::Filter,
      &Occur::MustNot,
    ]);
    let should_slice = should_v.unwrap().as_mut_slice();
    let must_slice = must_v.unwrap().as_mut_slice();
    let filter_slice = filter_v.unwrap().as_mut_slice();
    let must_not_slice = must_not_v.unwrap().as_mut_slice();
    if self.min_should_match > 0 {
      let req = Self::excl(
        Self::req(
          filter_slice,
          must_slice,
          lead_cost,
          false,
          context,
          searcher,
          &self.score_mode,
        )?,
        must_not_slice,
        lead_cost,
        context,
        searcher,
      )?;

      let opt = Self::opt(
        should_slice,
        self.min_should_match,
        self.score_mode,
        lead_cost,
        false,
        context,
        searcher,
      )?;
      let v = ConjunctionScorer::new(vec![req, opt], vec![0, 1])?;
      Ok(Box::new(v))
    } else {
      debug_assert!(self.score_mode.needs_scores());
      let req = Self::excl(
        Self::req(
          filter_slice,
          must_slice,
          lead_cost,
          false,
          context,
          searcher,
          &self.score_mode,
        )?,
        must_not_slice,
        lead_cost,
        context,
        searcher,
      )?;

      let opt = Self::opt(
        should_slice,
        self.min_should_match,
        self.score_mode,
        lead_cost,
        false,
        context,
        searcher,
      )?;

      let v = ReqOptSumScorer::new(req, opt, self.score_mode)?;
      Ok(Box::new(v))
    }
  }
  #[allow(clippy::type_complexity)]
  pub(crate) fn boolean_scorer(
    &mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Option<QueryWeightSsBulkScorer>> {
    let num_optional = self.subs.get(&Occur::Should).map(|v| v.len()).unwrap_or(0);
    let num_must = self.subs.get(&Occur::Must).map(|v| v.len()).unwrap_or(0);
    let num_required = num_must + self.subs.get(&Occur::Filter).map(|v| v.len()).unwrap_or(0);

    let mut positive = if num_required == 0 {
      let cost_threshold: i64 = if self.min_should_match <= 1 {
        // when all clauses are optional, use BooleanScorer aggressively
        -1
      } else {
        // when a minimum number of clauses should match, BooleanScorer is
        // going to score all windows that have at least minNrShouldMatch
        // matches in the window. But there is no way to know if there is
        // an intersection (all clauses might match a different doc ID and
        // there will be no matches in the end) so we should only use
        // BooleanScorer if matches are very dense
        (self.max_doc as i64) / 3
      };

      if self.cost(context, searcher)? < cost_threshold {
        return Ok(None);
      }
      match self.optional_bulk_scorer(context, searcher)? {
        Some(v) => v,
        None => return Ok(None),
      }
    } else if num_must == 0 && num_optional > 1 && self.min_should_match >= 1 {
      match self.filtered_optional_bulk_scorer(context, searcher)? {
        Some(v) => v,
        None => return Ok(None),
      }
    } else if num_required > 0 && num_optional == 0 && self.min_should_match == 0 {
      match self.required_bulk_scorer(context, searcher)? {
        Some(v) => v,
        None => return Ok(None),
      }
    } else {
      return Ok(None);
    };

    let positive_cost = positive.cost()?;

    let prohibited_suppliers = self
      .subs
      .get_mut(&Occur::MustNot)
      .map(|v| v.as_mut_slice())
      .unwrap_or(&mut []);
    if prohibited_suppliers.is_empty() {
      return Ok(Some(positive));
    }

    let mut prohibited = Vec::with_capacity(prohibited_suppliers.len());
    for ss in prohibited_suppliers.iter_mut() {
      prohibited.push(ss.get(positive_cost, context, searcher)?);
    }

    let prohibited_scorer = if prohibited.len() == 1 {
      ScorerEnum2::A(prohibited.pop().unwrap())
    } else {
      ScorerEnum2::B(DisjunctionScorer::new(
        prohibited,
        ScoreMode::CompleteNoScores,
        DisjunctionSumScorer,
      )?)
    };
    let v = ReqExclBulkScorer::new(positive, prohibited_scorer);
    Ok(Some(Box::new(v)))
  }
  #[allow(clippy::type_complexity)]
  fn optional_bulk_scorer(
    &mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Option<QueryWeightSsBulkScorer>> {
    let should_len = self.subs.get(&Occur::Should).map(|v| v.len()).unwrap_or(0);

    if should_len == 0 {
      return Ok(None);
    } else if should_len == 1 && self.min_should_match <= 1 {
      return match self.subs.get_mut(&Occur::Should).unwrap()[0].bulk_scorer(context, searcher)? {
        None => Ok(None),
        Some(bs) => return Ok(Some(bs)),
      };
    }

    if self.score_mode == ScoreMode::TopScores && self.min_should_match <= 1 {
      let mut optional_scorers = Vec::with_capacity(should_len);
      for ss in self.subs.get_mut(&Occur::Should).unwrap().iter_mut() {
        optional_scorers.push(ss.get(i64::MAX, context, searcher)?);
      }
      let v = Box::new(MaxScoreBulkScorer::with_no_filter(
        self.max_doc,
        optional_scorers,
      )?);
      return Ok(Some(v));
    }

    let mut optional = Vec::with_capacity(should_len);
    for ss in self.subs.get_mut(&Occur::Should).unwrap().iter_mut() {
      optional.push(ss.get(i64::MAX, context, searcher)?);
    }

    let msm = std::cmp::max(1, self.min_should_match);
    let v = Box::new(BooleanScorer::new(
      optional,
      msm as usize,
      self.score_mode.needs_scores(),
    )?);
    Ok(Some(v))
  }
  fn filtered_optional_bulk_scorer(
    &mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Option<QueryWeightSsBulkScorer>> {
    let must_len = self.subs.get(&Occur::Must).map(|v| v.len()).unwrap_or(0);
    let filter_len = self.subs.get(&Occur::Filter).map(|v| v.len()).unwrap_or(0);
    let should_len = self.subs.get(&Occur::Should).map(|v| v.len()).unwrap_or(0);

    if must_len != 0
      || filter_len == 0
      || self.score_mode != ScoreMode::TopScores
      || should_len <= 1
      || self.min_should_match > 1
    {
      return Ok(None);
    }

    let cost = self.cost(context, searcher)?;

    let mut optional_scorers = Vec::with_capacity(should_len);
    if let Some(v) = self.subs.get_mut(&Occur::Should) {
      for ss in v.iter_mut() {
        optional_scorers.push(ss.get(cost, context, searcher)?);
      }
    }

    let mut filters = Vec::with_capacity(filter_len);
    if let Some(v) = self.subs.get_mut(&Occur::Filter) {
      for ss in v.iter_mut() {
        filters.push(ss.get(cost, context, searcher)?);
      }
    }

    let filter_scorer = if filters.len() == 1 {
      ScorerEnum2::A(filters.pop().unwrap())
    } else {
      ScorerEnum2::B(ConjunctionScorer::new(filters, vec![])?)
    };

    let v = Box::new(MaxScoreBulkScorer::new(
      self.max_doc,
      optional_scorers,
      Some(filter_scorer),
    )?);
    Ok(Some(v))
  }
  #[allow(clippy::type_complexity)]
  fn required_bulk_scorer(
    &mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Option<QueryWeightSsBulkScorer>> {
    let must_len = {
      self
        .subs
        .get_mut(&Occur::Must)
        .map(|v| v.len())
        .unwrap_or(0)
    };
    let filter_len = self
      .subs
      .get_mut(&Occur::Filter)
      .map(|v| v.len())
      .unwrap_or(0);

    let required_cnt = must_len + filter_len;

    // no required clauses
    if required_cnt == 0 {
      return Ok(None);
    }

    if required_cnt == 1 {
      return if must_len != 0 {
        let must = self.subs.get_mut(&Occur::Must).unwrap();
        must[0].bulk_scorer(context, searcher)
      } else {
        let filter = self.subs.get_mut(&Occur::Filter).unwrap();
        if self.score_mode.needs_scores() {
          match filter[0].bulk_scorer(context, searcher)? {
            None => Err(LuceneError::illegal_state("bulk_scorer is None"))?,
            Some(s) => {
              let v = disable_scoring(s);
              Ok(Some(Box::new(v)))
            },
          }
        } else {
          filter[0].bulk_scorer(context, searcher)
        }
      };
    }

    let mut lead_cost = i64::MAX;
    match self.subs.get_mut(&Occur::Must) {
      Some(v) if !v.is_empty() => {
        for ss in v.iter_mut() {
          lead_cost = lead_cost.min(ss.cost(context, searcher)?);
        }
      },
      _ => {},
    }
    match self.subs.get_mut(&Occur::Filter) {
      Some(v) if !v.is_empty() => {
        for ss in v.iter_mut() {
          lead_cost = lead_cost.min(ss.cost(context, searcher)?);
        }
      },
      _ => {},
    }

    let mut required_no_scoring = Vec::with_capacity(filter_len);
    if let Some(v) = self.subs.get_mut(&Occur::Filter) {
      for ss in v.iter_mut() {
        required_no_scoring.push(ss.get(lead_cost, context, searcher)?);
      }
    }

    let mut required_scoring = Vec::with_capacity(must_len);
    if let Some(v) = self.subs.get_mut(&Occur::Must) {
      for ss in v.iter_mut() {
        if must_len == 1 {
          ss.set_top_level_scoring_clause()?;
        }
        required_scoring.push(ss.get(lead_cost, context, searcher)?);
      }
    }

    if self.score_mode == ScoreMode::TopScores && required_scoring.len() > 1 {
      let mut all_no_scoring_no_two_phase = true;
      for s in required_no_scoring.iter_mut() {
        if s.two_phase_iterator().is_some() {
          all_no_scoring_no_two_phase = false;
          break;
        }
      }

      let mut all_scoring_no_two_phase = true;
      for s in required_scoring.iter_mut() {
        if s.two_phase_iterator().is_some() {
          all_scoring_no_two_phase = false;
          break;
        }
      }

      if all_no_scoring_no_two_phase && all_scoring_no_two_phase {
        // Turn all filters into scoring clauses with a score of zero
        let mut wrap_required_scoring =
          Vec::with_capacity(required_no_scoring.len() + required_scoring.len());
        for x in required_scoring.into_iter() {
          wrap_required_scoring.push(ScorerEnum2::A(x))
        }
        for filter_scorer in required_no_scoring {
          wrap_required_scoring.push(ScorerEnum2::B(ConstantScoreScorer::from_disi(
            0.0,
            ScoreMode::Complete,
            Box::new(filter_scorer).take_iterator(),
          )));
        }
        return Ok(Some(Box::new(BlockMaxConjunctionBulkScorer::new(
          self.max_doc,
          wrap_required_scoring,
        )?)));
      }
    }

    if self.score_mode != ScoreMode::TopScores
      && required_scoring.len() + required_no_scoring.len() >= 2
    {
      let mut all_scoring_no_two_phase = true;
      for s in required_scoring.iter_mut() {
        if s.two_phase_iterator().is_some() {
          all_scoring_no_two_phase = false;
          break;
        }
      }

      let mut all_no_scoring_no_two_phase = true;
      for s in required_no_scoring.iter_mut() {
        if s.two_phase_iterator().is_some() {
          all_no_scoring_no_two_phase = false;
          break;
        }
      }

      if all_scoring_no_two_phase && all_no_scoring_no_two_phase {
        return Ok(Some(Box::new(ConjunctionBulkScorer::new(
          required_scoring,
          required_no_scoring,
        )?)));
      }
    }

    let mut required_scoring =
      if self.score_mode == ScoreMode::TopScores && required_scoring.len() > 1 {
        let v = BlockMaxConjunctionScorer::new(required_scoring)?;
        vec![ScorerEnum2::B(v)]
      } else {
        required_scoring.into_iter().map(ScorerEnum2::A).collect()
      };
    let mut required_no_scoring: Vec<
      ScorerEnum2<QueryWeightSsScorer, BlockMaxConjunctionScorer<QueryWeightSsScorer>>,
    > = required_no_scoring
      .into_iter()
      .map(ScorerEnum2::A)
      .collect();

    let conjunction_scorer = if required_scoring.len() + required_no_scoring.len() == 1 {
      if required_scoring.len() == 1 {
        let v = match required_scoring.pop() {
          Some(v) => v,
          None => {
            return Err(LuceneError::illegal_state(
              "required_scoring should not be empty",
            ));
          },
        };
        ScorerEnum3::A(v)
      } else {
        let inner = match required_no_scoring.pop() {
          Some(v) => v,
          None => {
            return Err(LuceneError::illegal_state(
              "required_no_scoring should not be empty",
            ));
          },
        };
        if self.score_mode.needs_scores() {
          ScorerEnum3::B(FilterScorerImpl::new(inner))
        } else {
          ScorerEnum3::A(inner)
        }
      }
    } else {
      let mut required = Vec::with_capacity(required_scoring.len() + required_no_scoring.len());
      let scoring_scorers_idx = (0..required_scoring.len()).collect::<Vec<_>>();
      required.extend(required_scoring);
      required.extend(required_no_scoring);
      ScorerEnum3::C(ConjunctionScorer::new(required, scoring_scorers_idx)?)
    };

    Ok(Some(Box::new(DefaultBulkScorer::new(conjunction_scorer))))
  }
  /// Create a new scorer for the given required clauses.
  /// Note that requiredScoring is a subset of required containing required clauses that should participate in scoring.
  fn req(
    required_no_scoring: &mut [QueryWeightSs<IRC>],
    required_scoring: &mut [QueryWeightSs<IRC>],
    lead_cost: i64,
    top_level_scoring_clause: bool,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
    score_mode: &ScoreMode,
  ) -> Result<QueryWeightSsScorer> {
    if required_no_scoring.len() + required_scoring.len() == 1 {
      let req = if required_no_scoring.is_empty() {
        required_scoring[0].get(lead_cost, context, searcher)?
      } else {
        required_no_scoring[0].get(lead_cost, context, searcher)?
      };

      if !score_mode.needs_scores() {
        return Ok(req);
      }

      if required_scoring.is_empty() {
        // Scores are needed but we only have a filter clause
        // BooleanWeight expects that calling score() is ok so we need to wrap
        // to prevent score() from being propagated
        return Ok(Box::new(FilterScorerImpl::new(req)));
      }

      return Ok(req);
    }

    let mut required_scorers =
      Vec::with_capacity(required_no_scoring.len() + required_scoring.len());
    let mut scoring_scorers = Vec::with_capacity(required_scoring.len());

    for s in required_no_scoring.iter_mut() {
      required_scorers.push(ScorerEnum2::A(s.get(lead_cost, context, searcher)?));
    }

    for s in required_scoring.iter_mut() {
      let scorer = s.get(lead_cost, context, searcher)?;
      scoring_scorers.push(scorer);
    }
    let scoring_scorers_idx = if *score_mode == ScoreMode::TopScores
      && scoring_scorers.len() > 1
      && top_level_scoring_clause
    {
      let block_max_scorer = BlockMaxConjunctionScorer::new(scoring_scorers)?;
      if required_scorers.is_empty() {
        return Ok(Box::new(block_max_scorer));
      } else {
        required_scorers.push(ScorerEnum2::B(block_max_scorer));
      }
      vec![required_scorers.len() - 1]
    } else {
      let base = required_scorers.len();
      let scoring_scorers_idx = (0..scoring_scorers.len())
        .map(|i| base + i)
        .collect::<Vec<_>>();
      required_scorers.extend(scoring_scorers.into_iter().map(ScorerEnum2::A));
      scoring_scorers_idx
    };
    let v = ConjunctionScorer::new(required_scorers, scoring_scorers_idx)?;
    Ok(Box::new(v))
  }
  fn excl(
    main: QueryWeightSsScorer,
    prohibited: &mut [QueryWeightSs<IRC>],
    lead_cost: i64,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<QueryWeightSsScorer> {
    if prohibited.is_empty() {
      Ok(main)
    } else {
      let opt = Self::opt(
        prohibited,
        1,
        CompleteNoScores,
        lead_cost,
        false,
        context,
        searcher,
      )?;
      Ok(Box::new(ReqExclScorer::new(main, opt)?))
    }
  }

  fn opt(
    optional: &mut [QueryWeightSs<IRC>],
    min_should_match: i32,
    score_mode: ScoreMode,
    lead_cost: i64,
    top_level_scoring_clause: bool,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<QueryWeightSsScorer> {
    if optional.len() == 1 {
      return optional[0].get(lead_cost, context, searcher);
    }

    let mut optional_scorers = Vec::with_capacity(optional.len());
    for supplier in optional.iter_mut() {
      optional_scorers.push(supplier.get(lead_cost, context, searcher)?);
    }
    // Technically speaking, WANDScorer should be able to handle the following 3 conditions now
    // 1. Any ScoreMode (with scoring or not)
    // 2. Any minCompetitiveScore ( >= 0 )
    // 3. Any minShouldMatch ( >= 0 )
    //
    // However, as WANDScorer uses more complex algorithm and data structure, we would like to
    // still use DisjunctionSumScorer to handle exhaustive pure disjunctions, which may be faster
    if (score_mode == ScoreMode::TopScores && top_level_scoring_clause) || min_should_match > 1 {
      Ok(Box::new(WANDScorer::new(
        optional_scorers,
        min_should_match,
        score_mode,
        lead_cost,
      )?))
    } else {
      Ok(Box::new(DisjunctionScorer::new(
        optional_scorers,
        score_mode,
        DisjunctionSumScorer,
      )?))
    }
  }
}

pub type FilteredOptionalBulkScorer<S> =
  MaxScoreBulkScorer<S, ScorerEnum2<S, ConjunctionScorer<S>>>;

impl<IRC> ScorerSupplier<IRC> for BooleanScorerSupplier<IRC>
where
  IRC: IndexReaderContext + 'static,
{
  // type Scorer = GetType<SsScorer<LR>>;
  type Scorer = QueryWeightSsScorer;
  type BulkScorer = QueryWeightSsBulkScorer;

  fn get(
    &mut self,
    lead_cost: i64,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Self::Scorer> {
    let scorer = self.get_internal(lead_cost, context, searcher)?;

    if self.score_mode == ScoreMode::TopScores {
      let should_empty = self
        .subs
        .get(&Occur::Should)
        .map(|v| v.is_empty())
        .unwrap_or(true);
      let must_empty = self
        .subs
        .get(&Occur::Must)
        .map(|v| v.is_empty())
        .unwrap_or(true);

      if should_empty && must_empty {
        // no scoring clauses but scores are needed so we wrap the scorer in
        // a constant score in order to allow early termination
        if scorer.two_phase_iterator().is_some() {
          let tpi = match Box::new(scorer).take_two_phase_iterator() {
            Some(v) => v,
            None => return Err(LuceneError::illegal_state("already taken?")),
          };
          return Ok(Box::new(ConstantScoreScorer::from_tpi(
            0.0,
            self.score_mode,
            tpi,
          )));
        } else {
          let disi = Box::new(scorer).take_iterator();
          return Ok(Box::new(ConstantScoreScorer::from_disi(
            0.0,
            self.score_mode,
            disi,
          )));
        };
      }
    }
    Ok(scorer)
  }

  fn bulk_scorer(
    &mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Option<Self::BulkScorer>> {
    if let Some(bs) = self.boolean_scorer(context, searcher)? {
      return Ok(Some(Box::new(bs)));
    }

    // use a Scorer-based impl (BS2)
    match self.default_bulk_scorer(context, searcher).map(Some)? {
      Some(v) => Ok(Some(Box::new(v))),
      None => Ok(None),
    }
  }

  fn cost(
    &mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<i64> {
    if self.cost == -1 {
      self.cost = self.compute_cost(context, searcher)?;
    }
    Ok(self.cost)
  }

  fn set_top_level_scoring_clause(&mut self) -> Result<()> {
    self.top_level_scoring_clause = true;

    let should_len = self.subs.get(&Occur::Should).map(|v| v.len()).unwrap_or(0);
    let must_len = self.subs.get(&Occur::Must).map(|v| v.len()).unwrap_or(0);

    if should_len + must_len == 1 {
      // If there is a single scoring clause, propagate the call.
      if let Some(v) = self.subs.get_mut(&Occur::Should) {
        for ss in v.iter_mut() {
          ss.set_top_level_scoring_clause()?;
        }
      }
      if let Some(v) = self.subs.get_mut(&Occur::Must) {
        for ss in v.iter_mut() {
          ss.set_top_level_scoring_clause()?;
        }
      }
    }
    Ok(())
  }
  #[cfg(test)]
  fn as_any(&mut self) -> &mut dyn Any {
    self
  }
}

pub struct FilterScorerImpl<S> {
  inner: S,
}
impl<S> FilterScorerImpl<S> {
  fn new(inner: S) -> Self {
    Self { inner }
  }
}

impl<S> Scorable for FilterScorerImpl<S>
where
  S: Scorer,
{
  fn score(&mut self) -> Result<f32> {
    Ok(0f32)
  }

  fn cost(&self) -> Result<i64> {
    self.iterator().cost()
  }
}

impl<S> crate::core::search::scorable::FixedScore for FilterScorerImpl<S> {}

impl<S> Scorer for FilterScorerImpl<S>
where
  S: Scorer,
{
  fn doc_id(&mut self) -> Result<i32> {
    self.inner.doc_id()
  }

  fn iterator(&self) -> Box<dyn DocIdSetIterator + '_> {
    self.inner.iterator()
  }

  fn iterator_mut(&mut self) -> Box<dyn DocIdSetIterator + '_> {
    self.inner.iterator_mut()
  }

  fn take_iterator(self: Box<Self>) -> Box<dyn DocIdSetIterator> {
    let FilterScorerImpl { inner: base } = *self;
    Box::new(base).take_iterator()
  }

  fn two_phase_iterator(&self) -> Option<Box<dyn TwoPhaseIterator + '_>> {
    self.inner.two_phase_iterator()
  }

  fn two_phase_iterator_mut(&mut self) -> Option<Box<dyn TwoPhaseIterator + '_>> {
    self.inner.two_phase_iterator_mut()
  }

  fn take_two_phase_iterator(self: Box<Self>) -> Option<Box<dyn TwoPhaseIterator>>
  where
    Self: Sized,
  {
    let FilterScorerImpl { inner: base } = *self;
    Box::new(base).take_two_phase_iterator()
  }

  fn advance_shallow(&mut self, target: i32) -> Result<i32> {
    self.inner.advance_shallow(target)
  }

  fn default_advance_shallow(&mut self, target: i32) -> Result<i32> {
    self.inner.default_advance_shallow(target)
  }

  fn get_max_score(&mut self, _up_to: i32) -> Result<f32> {
    Ok(0f32)
  }

  fn default_cost(&mut self) -> Result<i64> {
    self.inner.default_cost()
  }

  fn has_two_phase_iterator(&self) -> TwoPhaseState {
    self.inner.has_two_phase_iterator()
  }

  fn approximation(&self) -> Box<dyn DocIdSetIterator + '_> {
    self.inner.approximation()
  }

  fn approximation_mut(&mut self) -> Box<dyn DocIdSetIterator + '_> {
    self.inner.approximation_mut()
  }
}

pub(crate) fn disable_scoring<BS>(scorer: BS) -> BulkScorerImpl<BS> {
  BulkScorerImpl::new(scorer)
}

pub struct BulkScorerImpl<BS> {
  scorer: BS,
}
impl<BS> BulkScorerImpl<BS> {
  fn new(scorer: BS) -> Self {
    Self { scorer }
  }
}
impl<BS> BulkScorer for BulkScorerImpl<BS>
where
  BS: BulkScorer,
{
  fn score(
    &mut self,
    collector: &mut dyn LeafCollector,
    accept_docs: Option<&dyn Bits>,
    min: i32,
    max: i32,
  ) -> Result<i32> {
    let mut no_score_collector = LeafCollectorImpl::new(collector);
    self
      .scorer
      .score(&mut no_score_collector, accept_docs, min, max)
  }

  fn cost(&mut self) -> Result<i64> {
    self.scorer.cost()
  }
}

pub struct LeafCollectorImpl<LC> {
  collector: LC,
  fake: Score,
}
impl<LC> LeafCollectorImpl<LC> {
  fn new(collector: LC) -> Self {
    Self {
      collector,
      fake: Score::new(0.0),
    }
  }
}

impl<LC> Display for LeafCollectorImpl<LC>
where
  LC: Display,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "LeafCollectorImpl({})", self.collector)
  }
}

impl<LC> LeafCollector for LeafCollectorImpl<LC>
where
  LC: LeafCollector,
{
  fn set_scorer(&mut self, _scorer: &mut dyn Scorable) -> Result<()> {
    self.collector.set_scorer(&mut self.fake)
  }

  fn collect(&mut self, doc: i32, _scorer: &mut dyn Scorable) -> Result<()> {
    self.collector.collect(doc, &mut self.fake)
  }
  fn competitive_iterator(&mut self) -> Result<Option<Box<dyn DocIdSetIterator + '_>>> {
    self.collector.competitive_iterator()
  }

  fn finish(&mut self) -> Result<()> {
    self.collector.finish()
  }
}
