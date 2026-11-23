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

//! Built-in bulk scoring with concrete collectors across recursive query nodes.
use crate::core::search::approximation_disi::ApproximationDISI;
use crate::core::search::boolean_scorer_supplier::BulkScorerImpl;
use crate::core::search::bulk_scorer::{BulkScorer, BulkScorerEnum2};
use crate::core::search::constant_score_query::{ConstantBulkScorer, FilterScorableImpl};
use crate::core::search::doc_id_stream::DocIdStream;
use crate::core::search::leaf_collector::LeafCollector;
use crate::core::search::query_scorer::{
  BuiltinQueryScorer, DefaultQueryBulkScorer, IntoQueryScorer,
};
use crate::core::search::req_excl_bulk_scorer::ReqExclBulkScorer;
use crate::core::search::scorable::Scorable;
use crate::core::search::scorer::{Scorer, TwoPhaseState};
use crate::core::search::scorer_types::ScorerTypes;
use crate::core::search::two_phase_iterator::TwoPhaseIterator;
use crate::core::search::weight::DefaultBulkScorer;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::Result;

pub struct BuiltinQueryBulkScorer<ST: ScorerTypes> {
  inner: BulkNode<ST>,
}
pub trait IntoQueryBulkScorer<ST: ScorerTypes> {
  fn into_query_bulk_scorer(self) -> BuiltinQueryBulkScorer<ST>;

  /// Consume an existing box, retaining its allocation when the destination is boxed.
  fn into_query_bulk_scorer_from_box(self: Box<Self>) -> BuiltinQueryBulkScorer<ST>
  where
    Self: Sized,
  {
    (*self).into_query_bulk_scorer()
  }
}

macro_rules! bulk_nodes {
  (boxed { $( $(#[$attrs:meta])* $variant:ident: $ty:ty, )+ }
   inline { $( $inline:ident: $inline_ty:ty, )+ }) => {
    enum BulkNode<ST: ScorerTypes> {
      #[cfg(test)] Fixture(Box<dyn crate::test_framework::core::search::test_collector::TestBulkScorer>),
      Default(DefaultQueryBulkScorer<ST>),
      $( $(#[$attrs])* $variant(Box<$ty>), )+
      $( $inline($inline_ty), )+
      Constant { inner: Box<BuiltinQueryBulkScorer<ST>>, score: f32 },
      Disabled(Box<BuiltinQueryBulkScorer<ST>>),
      RequiredExcluded { required: Box<BuiltinQueryBulkScorer<ST>>, excluded: BuiltinQueryScorer<ST> },
    }
    $( $(#[$attrs])* impl<ST: ScorerTypes> IntoQueryBulkScorer<ST> for $ty {
      fn into_query_bulk_scorer(self) -> BuiltinQueryBulkScorer<ST> {
        BuiltinQueryBulkScorer { inner: BulkNode::$variant(Box::new(self)) }
      }
      fn into_query_bulk_scorer_from_box(self: Box<Self>) -> BuiltinQueryBulkScorer<ST> {
        BuiltinQueryBulkScorer { inner: BulkNode::$variant(self) }
      }
    } )+
    $( impl<ST: ScorerTypes> IntoQueryBulkScorer<ST> for $inline_ty {
      fn into_query_bulk_scorer(self) -> BuiltinQueryBulkScorer<ST> {
        BuiltinQueryBulkScorer { inner: BulkNode::$inline(self) }
      }
    } )+
    impl<ST: ScorerTypes> BuiltinQueryBulkScorer<ST> {
      fn score_internal<CLC: LeafCollector, AB: Bits + ?Sized>(&mut self, collector: &mut CLC, accept_docs: Option<&AB>, min: i32, max: i32, overlay: CollectorOverlay) -> Result<i32> {
        match &mut self.inner {
          $( $(#[$attrs])* BulkNode::$variant(scorer) => scorer.score(&mut OverlayCollector { collector, overlay, fake: crate::core::search::score::Score::new(0.0) }, accept_docs, min, max), )+
          $( BulkNode::$inline(scorer) => scorer.score(&mut OverlayCollector { collector, overlay, fake: crate::core::search::score::Score::new(0.0) }, accept_docs, min, max), )+
          #[cfg(test)] BulkNode::Fixture(scorer) => scorer.score(&mut OverlayCollector { collector, overlay, fake: crate::core::search::score::Score::new(0.0) }, accept_docs, min, max),
          BulkNode::Default(scorer) if !overlay.disabled && overlay.fixed_score.is_none() => {
            scorer.score(collector, accept_docs, min, max)
          },
          BulkNode::Default(scorer) => scorer.score(&mut OverlayCollector { collector, overlay, fake: crate::core::search::score::Score::new(0.0) }, accept_docs, min, max),
          BulkNode::Constant { inner, score } => {
            let overlay = overlay.with_constant(*score);
            inner.score_internal(collector, accept_docs, min, max, overlay)
          },
          BulkNode::Disabled(inner) => inner.score_internal(collector, accept_docs, min, max, overlay.with_disabled()),
          BulkNode::RequiredExcluded { required, excluded } => {
            let mut upto = min;
            let mut excluded_doc = excluded.approx_doc_id();
            let excluded_has_two_phase = excluded.has_two_phase_iterator() == TwoPhaseState::Yes;
            while upto < max {
              if excluded_doc < upto { excluded_doc = excluded.approx_advance(upto)?; }
              if excluded_doc == upto {
                if !excluded_has_two_phase || excluded.matches()? { upto += 1; }
                excluded_doc = excluded.approx_next_doc()?;
              } else {
                upto = required.score_internal(collector, accept_docs, upto, excluded_doc.min(max), overlay)?;
              }
            }
            if upto == max { upto = required.score_internal(collector, accept_docs, upto, upto, overlay)?; }
            Ok(upto)
          }
        }
      }
    }
    impl<ST: ScorerTypes> BulkScorer for BuiltinQueryBulkScorer<ST> {
      fn score<CLC: LeafCollector, AB: Bits + ?Sized>(&mut self, collector: &mut CLC, accept_docs: Option<&AB>, min: i32, max: i32) -> Result<i32> {
        self.score_internal(collector, accept_docs, min, max, CollectorOverlay::default())
      }
      fn cost(&mut self) -> Result<i64> {
        match &mut self.inner {
          $( $(#[$attrs])* BulkNode::$variant(scorer) => scorer.cost(), )+
          #[cfg(test)] BulkNode::Fixture(scorer) => scorer.cost(),
          $( BulkNode::$inline(scorer) => scorer.cost(), )+
          BulkNode::Default(scorer) => scorer.cost(),
          BulkNode::Constant { inner, .. } | BulkNode::Disabled(inner) => inner.cost(),
          BulkNode::RequiredExcluded { required, .. } => required.cost(),
        }
      }
      #[cfg(test)]
      fn kind(&self) -> crate::core::search::bulk_scorer::BulkScorerKind {
        #[allow(unreachable_patterns)]
        match &self.inner {
          BulkNode::Fixture(scorer) => scorer.kind(),
          $( $(#[$attrs])* BulkNode::$variant(_) => crate::core::search::bulk_scorer::BulkScorerKind::Default, )+
          $( BulkNode::$inline(_) => crate::core::search::bulk_scorer::BulkScorerKind::Default, )+
          BulkNode::Default(_) => crate::core::search::bulk_scorer::BulkScorerKind::Default,
          BulkNode::Constant { inner, .. } | BulkNode::Disabled(inner) => inner.kind(),
          BulkNode::RequiredExcluded { .. } => crate::core::search::bulk_scorer::BulkScorerKind::ReqExcl,
        }
      }
    }
  }
}
bulk_nodes! {
  boxed {
    Boolean: crate::core::search::boolean_scorer::BooleanScorer<BuiltinQueryScorer<ST>>,
    BooleanTerm: crate::core::search::boolean_scorer::BooleanScorer<Box<crate::core::search::scorer::ScorerEnum2<crate::core::search::term_scorer::TermScorer<crate::core::search::term_scorer::PostingsTermState<ST::Posting, std::sync::Arc<crate::core::search::term_query::TermQuerySimScorer<ST::Similarity>>>, ST::Norm>, crate::core::search::constant_score_scorer::ConstantScoreDisiScorer<crate::core::search::doc_id_set_iterator::EmptyDISI>>>>,
    Conjunction: crate::core::search::conjunction_bulk_scorer::ConjunctionBulkScorer<BuiltinQueryScorer<ST>>,
    BlockMaxConjunction: crate::core::search::block_max_conjunction_bulk_scorer::BlockMaxConjunctionBulkScorer<BuiltinQueryScorer<ST>>,
    MaxScore: crate::core::search::max_score_bulk_scorer::MaxScoreBulkScorer<BuiltinQueryScorer<ST>, BuiltinQueryScorer<ST>>,
    MaxScoreUnfiltered: crate::core::search::max_score_bulk_scorer::MaxScoreBulkScorer<BuiltinQueryScorer<ST>, crate::core::search::dummy::dummy_scorer::DummyScorer>,
    DisjunctionMax: crate::core::search::disjunction_max_bulk_scorer::DisjunctionMaxBulkScorer<BuiltinQueryBulkScorer<ST>>,
  }
  inline {
    MatchAll: crate::core::search::match_all_docs_query::MatchAllBulkScorer,
  }
}
impl<ST: ScorerTypes> BuiltinQueryBulkScorer<ST> {
  pub fn new<BS: IntoQueryBulkScorer<ST>>(scorer: BS) -> Self {
    scorer.into_query_bulk_scorer()
  }
}
#[cfg(test)]
impl<ST: ScorerTypes> IntoQueryBulkScorer<ST>
  for Box<dyn crate::test_framework::core::search::test_collector::TestBulkScorer>
{
  fn into_query_bulk_scorer(self) -> BuiltinQueryBulkScorer<ST> {
    BuiltinQueryBulkScorer {
      inner: BulkNode::Fixture(self),
    }
  }
}

impl<ST: ScorerTypes> IntoQueryBulkScorer<ST> for BuiltinQueryBulkScorer<ST> {
  fn into_query_bulk_scorer(self) -> BuiltinQueryBulkScorer<ST> {
    self
  }
}
impl<ST: ScorerTypes> IntoQueryBulkScorer<ST> for DefaultQueryBulkScorer<ST> {
  fn into_query_bulk_scorer(self) -> BuiltinQueryBulkScorer<ST> {
    BuiltinQueryBulkScorer {
      inner: BulkNode::Default(self),
    }
  }
}
impl<ST: ScorerTypes, BS: IntoQueryBulkScorer<ST>> IntoQueryBulkScorer<ST> for Box<BS> {
  fn into_query_bulk_scorer(self) -> BuiltinQueryBulkScorer<ST> {
    BS::into_query_bulk_scorer_from_box(self)
  }
  fn into_query_bulk_scorer_from_box(self: Box<Self>) -> BuiltinQueryBulkScorer<ST> {
    (*self).into_query_bulk_scorer()
  }
}
impl<ST: ScorerTypes, S: Scorer + IntoQueryScorer<ST>> IntoQueryBulkScorer<ST>
  for DefaultBulkScorer<S>
{
  fn into_query_bulk_scorer(self) -> BuiltinQueryBulkScorer<ST> {
    self
      .scorer
      .into_query_scorer()
      .into_default_bulk()
      .into_query_bulk_scorer()
  }
}
impl<
  ST: ScorerTypes,
  A: BulkScorer + IntoQueryBulkScorer<ST>,
  B: BulkScorer + IntoQueryBulkScorer<ST>,
> IntoQueryBulkScorer<ST> for BulkScorerEnum2<A, B>
{
  fn into_query_bulk_scorer(self) -> BuiltinQueryBulkScorer<ST> {
    match self {
      Self::A(s) => s.into_query_bulk_scorer(),
      Self::B(s) => s.into_query_bulk_scorer(),
    }
  }
}
impl<ST: ScorerTypes, BS: IntoQueryBulkScorer<ST>> IntoQueryBulkScorer<ST>
  for ConstantBulkScorer<BS>
{
  fn into_query_bulk_scorer(self) -> BuiltinQueryBulkScorer<ST> {
    BuiltinQueryBulkScorer {
      inner: BulkNode::Constant {
        inner: Box::new(self.bulk_scorer.into_query_bulk_scorer()),
        score: self.the_score,
      },
    }
  }
}
impl<ST: ScorerTypes, BS: IntoQueryBulkScorer<ST>> IntoQueryBulkScorer<ST> for BulkScorerImpl<BS> {
  fn into_query_bulk_scorer(self) -> BuiltinQueryBulkScorer<ST> {
    BuiltinQueryBulkScorer {
      inner: BulkNode::Disabled(Box::new(self.scorer.into_query_bulk_scorer())),
    }
  }
}
impl<ST: ScorerTypes, BS: IntoQueryBulkScorer<ST>, S: IntoQueryScorer<ST>> IntoQueryBulkScorer<ST>
  for ReqExclBulkScorer<BS, S>
{
  fn into_query_bulk_scorer(self) -> BuiltinQueryBulkScorer<ST> {
    BuiltinQueryBulkScorer {
      inner: BulkNode::RequiredExcluded {
        required: Box::new(self.req.into_query_bulk_scorer()),
        excluded: self.excl.into_query_scorer(),
      },
    }
  }
}

/// Collapse recursive wrappers into state rather than growing the generic collector type.
#[derive(Clone, Copy, Default)]
struct CollectorOverlay {
  fixed_score: Option<f32>,
  disabled: bool,
}
impl CollectorOverlay {
  fn with_constant(mut self, score: f32) -> Self {
    if self.fixed_score.is_none() && !self.disabled {
      self.fixed_score = Some(score);
    }
    self
  }
  fn with_disabled(mut self) -> Self {
    self.disabled = true;
    self
  }
}
struct OverlayCollector<'a, C: LeafCollector> {
  collector: &'a mut C,
  overlay: CollectorOverlay,
  fake: crate::core::search::score::Score,
}
impl<C: LeafCollector> LeafCollector for OverlayCollector<'_, C> {
  type CompetitiveIterator<'a>
    = C::CompetitiveIterator<'a>
  where
    Self: 'a;
  fn set_scorer<SC: Scorable + ?Sized>(&mut self, scorer: &mut SC) -> Result<()> {
    match (self.overlay.disabled, self.overlay.fixed_score) {
      (true, Some(score)) => self
        .collector
        .set_scorer(&mut FilterScorableImpl::new(score, &mut self.fake)),
      (true, None) => self.collector.set_scorer(&mut self.fake),
      (false, Some(score)) => self
        .collector
        .set_scorer(&mut FilterScorableImpl::new(score, scorer)),
      (false, None) => self.collector.set_scorer(scorer),
    }
  }
  fn collect<SC: Scorable + ?Sized>(&mut self, doc: i32, scorer: &mut SC) -> Result<()> {
    match (self.overlay.disabled, self.overlay.fixed_score) {
      (true, Some(score)) => self
        .collector
        .collect(doc, &mut FilterScorableImpl::new(score, &mut self.fake)),
      (true, None) => self.collector.collect(doc, &mut self.fake),
      (false, Some(score)) => self
        .collector
        .collect(doc, &mut FilterScorableImpl::new(score, scorer)),
      (false, None) => self.collector.collect(doc, scorer),
    }
  }
  fn collect_stream<DS: DocIdStream + ?Sized, SC: Scorable + ?Sized>(
    &mut self,
    stream: &mut DS,
    scorer: &mut SC,
  ) -> Result<()> {
    if self.overlay.disabled {
      return self.default_collect_stream(stream, scorer);
    }
    match self.overlay.fixed_score {
      Some(score) => self
        .collector
        .collect_stream(stream, &mut FilterScorableImpl::new(score, scorer)),
      None => self.collector.collect_stream(stream, scorer),
    }
  }
  fn competitive_iterator(&mut self) -> Result<Option<Self::CompetitiveIterator<'_>>> {
    if self.overlay.disabled {
      Ok(None)
    } else {
      self.collector.competitive_iterator()
    }
  }
  fn finish(&mut self) -> Result<()> {
    if self.overlay.disabled {
      Ok(())
    } else {
      self.collector.finish()
    }
  }
}

impl<C: LeafCollector> std::fmt::Display for OverlayCollector<'_, C> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", self.collector)
  }
}

macro_rules! convert_bulk_enum {
  ($name:ident, $( $variant:ident: $ty:ident ),+) => {
    impl<ST: ScorerTypes, $( $ty: BulkScorer + IntoQueryBulkScorer<ST> ),+> IntoQueryBulkScorer<ST> for crate::core::search::bulk_scorer::$name<$( $ty ),+> {
      fn into_query_bulk_scorer(self) -> BuiltinQueryBulkScorer<ST> { match self { $( Self::$variant(scorer) => scorer.into_query_bulk_scorer(), )+ } }
    }
  };
}
convert_bulk_enum!(BulkScorerEnum4, A: A1, B: B1, C: C1, D: D1);

#[cfg(test)]
impl<ST: ScorerTypes> BuiltinQueryBulkScorer<ST> {
  pub(crate) fn for_test<S: BulkScorer + 'static>(scorer: S) -> Self {
    let scorer: Box<dyn crate::test_framework::core::search::test_collector::TestBulkScorer> =
      Box::new(scorer);
    scorer.into_query_bulk_scorer()
  }
}
