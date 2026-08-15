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
use crate::core::search::bulk_scorer::BulkScorer;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::leaf_collector::LeafCollector;
use crate::core::search::scorable::{FixedScore, Scorable};
use crate::core::search::scorer::{Scorer, TwoPhaseState};
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::Result;
use std::cell::RefCell;
use std::fmt::{Display, Formatter};

/// A BulkScorer-backed scorer.
pub struct BulkScorerWrapperScorer<BS> {
  disi: DocIdSetIteratorImpl<BS>,
}
impl<BS> BulkScorerWrapperScorer<BS>
where
  BS: BulkScorer,
{
  pub fn new(scorer: BS, buffer_size: usize) -> Self {
    let disi = DocIdSetIteratorImpl::new(scorer, buffer_size);
    Self { disi }
  }
}

impl<BS> Scorable for BulkScorerWrapperScorer<BS>
where
  BS: BulkScorer,
{
  fn score(&mut self) -> Result<f32> {
    Ok(self.disi.scores[self.disi.i as usize])
  }
}

impl<BS> FixedScore for BulkScorerWrapperScorer<BS> where BS: BulkScorer {}

impl<BS> Scorer for BulkScorerWrapperScorer<BS>
where
  BS: BulkScorer + 'static,
{
  fn doc_id(&mut self) -> Result<i32> {
    Ok(self.disi.doc)
  }

  fn iterator(&self) -> Box<dyn DocIdSetIterator + '_> {
    Box::new(&self.disi)
  }

  fn iterator_mut(&mut self) -> Box<dyn DocIdSetIterator + '_> {
    Box::new(&mut self.disi)
  }

  fn take_iterator(self: Box<Self>) -> Box<dyn DocIdSetIterator> {
    let BulkScorerWrapperScorer { disi } = *self;
    Box::new(disi)
  }

  fn get_max_score(&mut self, _upto: i32) -> Result<f32> {
    Ok(f32::INFINITY)
  }

  fn has_two_phase_iterator(&self) -> TwoPhaseState {
    TwoPhaseState::No
  }

  fn approximation(&self) -> Box<dyn DocIdSetIterator + '_> {
    Box::new(&self.disi)
  }

  fn approximation_mut(&mut self) -> Box<dyn DocIdSetIterator + '_> {
    Box::new(&mut self.disi)
  }
}

struct LeafCollectorImpl<'a> {
  docs: &'a mut [i32],
  scores: &'a mut [f32],
  buffer_length: &'a mut usize,
}
impl<'a> LeafCollectorImpl<'a> {
  pub fn new(docs: &'a mut [i32], scores: &'a mut [f32], buffer_length: &'a mut usize) -> Self {
    Self {
      docs,
      scores,
      buffer_length,
    }
  }
}

impl Display for LeafCollectorImpl<'_> {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}

impl<'a> LeafCollector for LeafCollectorImpl<'a> {
  fn collect(
    &mut self,
    doc: i32,
    scorer: &mut dyn Scorable,
  ) -> crate::core::util::error::lucene_error::Result<()> {
    self.docs[*self.buffer_length] = doc;
    self.scores[*self.buffer_length] = scorer.score()?;
    *self.buffer_length += 1;
    Ok(())
  }
}

struct DocIdSetIteratorImpl<BS> {
  scorer: RefCell<BS>,
  i: i32,
  doc: i32,
  next: i32,
  docs: Vec<i32>,
  scores: Vec<f32>,
  buffer_length: usize,
}
impl<BS> DocIdSetIteratorImpl<BS>
where
  BS: BulkScorer,
{
  pub fn new(scorer: BS, buffer_size: usize) -> Self {
    Self {
      scorer: RefCell::new(scorer),
      i: 0,
      doc: -1,
      next: 0,
      docs: vec![0; buffer_size],
      scores: vec![0.0; buffer_size],
      buffer_length: 0,
    }
  }
  fn refill(&mut self, target: i32) -> Result<()> {
    self.buffer_length = 0;

    while self.next != NO_MORE_DOCS && self.buffer_length == 0 {
      let min = target.max(self.next);
      let max = min + self.docs.len() as i32;

      let mut collector =
        LeafCollectorImpl::new(&mut self.docs, &mut self.scores, &mut self.buffer_length);

      self.next = self
        .scorer
        .borrow_mut()
        .score(&mut collector, None::<&dyn Bits>, min, max)?;
    }

    self.i = -1;
    Ok(())
  }
}
impl<BS> DocIdSetIterator for DocIdSetIteratorImpl<BS>
where
  BS: BulkScorer,
{
  fn doc_id(&self) -> i32 {
    self.doc
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.advance(self.doc + 1)
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    if self.buffer_length == 0 || self.docs[self.buffer_length - 1] < target {
      self.refill(target)?;
    }

    let start = (self.i + 1) as usize;
    let end = self.buffer_length;

    let slice = &self.docs[start..end];

    let pos = match slice.binary_search(&target) {
      Ok(idx) => start + idx,
      Err(idx) => start + idx,
    };

    if pos == self.buffer_length {
      self.doc = NO_MORE_DOCS;
      return Ok(self.doc);
    }

    self.i = pos as i32;
    self.doc = self.docs[pos];

    Ok(self.doc)
  }

  fn cost(&self) -> Result<i64> {
    self.scorer.borrow_mut().cost()
  }
}
