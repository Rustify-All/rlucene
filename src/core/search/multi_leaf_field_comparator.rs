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
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::search::field_comparator::{FieldComparator, FieldComparatorEnum};
use crate::core::search::leaf_field_comparator::{
  LeafFieldComparator, LeafFieldComparatorDocIdSetIteratorRef, LeafFieldComparatorEnum,
};
use crate::core::search::scorable::Scorable;
use crate::core::util::error::lucene_error::{LuceneError, Result};

pub struct MultiLeafFieldComparator<LR>
where
  LR: LeafReader,
{
  first_comparator: LeafFieldComparatorEnum<LR>,
  first_reverse_mul: i32,
  comparators: Vec<(LeafFieldComparatorEnum<LR>, i32)>,
}
impl<LR> MultiLeafFieldComparator<LR>
where
  LR: LeafReader,
{
  pub(crate) fn new(
    comparators: &mut [FieldComparatorEnum],
    reverse_mul: &[i32],
    context: &LeafReaderContext<LR>,
  ) -> Result<Self> {
    if comparators.len() != reverse_mul.len() {
      return Err(LuceneError::illegal_argument(format!(
        "Must have the same number of comparators and reverse_mul, got {} and {}",
        comparators.len(),
        reverse_mul.len()
      )));
    }

    let mut leaf = Self {
      first_comparator: comparators[0].get_leaf_comparator(context)?,
      first_reverse_mul: reverse_mul[0],
      comparators: Vec::with_capacity(comparators.len() - 1),
    };
    for (comparator, &direction) in comparators[1..].iter_mut().zip(&reverse_mul[1..]) {
      leaf
        .comparators
        .push((comparator.get_leaf_comparator(context)?, direction));
    }
    Ok(leaf)
  }
  pub(crate) fn set_bottom(
    &mut self,
    slot: usize,
    comparators: &mut [FieldComparatorEnum],
  ) -> Result<()> {
    debug_assert_eq!(
      self.comparators.len() + 1,
      comparators.len(),
      "comparators length mismatch"
    );

    self
      .first_comparator
      .set_bottom(slot, &mut comparators[0])?;
    for ((comp, _), c) in self
      .comparators
      .iter_mut()
      .zip(comparators.iter_mut().skip(1))
    {
      comp.set_bottom(slot, c)?;
    }

    Ok(())
  }

  #[inline]
  pub(crate) fn compare_bottom<S>(
    &mut self,
    doc: i32,
    scorer: &mut S,
    comparators: &mut [FieldComparatorEnum],
  ) -> Result<i32>
  where
    S: Scorable + ?Sized,
  {
    debug_assert_eq!(
      self.comparators.len() + 1,
      comparators.len(),
      "comparators length mismatch"
    );
    let cmp = self.first_reverse_mul
      * self
        .first_comparator
        .compare_bottom(doc, scorer, &mut comparators[0])?;
    if cmp != 0 {
      return Ok(cmp);
    }
    for ((comp_self, reverse), comp_arg) in self
      .comparators
      .iter_mut()
      .zip(comparators.iter_mut().skip(1))
    {
      let cmp = *reverse * comp_self.compare_bottom(doc, scorer, comp_arg)?;
      if cmp != 0 {
        return Ok(cmp);
      }
    }

    Ok(0)
  }

  pub(crate) fn compare_top<S>(
    &mut self,
    doc: i32,
    scorer: &mut S,
    comparators: &mut [FieldComparatorEnum],
  ) -> Result<i32>
  where
    S: Scorable + ?Sized,
  {
    debug_assert_eq!(
      self.comparators.len() + 1,
      comparators.len(),
      "comparators length mismatch"
    );

    let mut cmp = self.first_reverse_mul
      * self
        .first_comparator
        .compare_top(doc, scorer, &mut comparators[0])?;
    if cmp != 0 {
      return Ok(cmp);
    }

    for ((comp_self, reverse), comp_arg) in self
      .comparators
      .iter_mut()
      .zip(comparators.iter_mut().skip(1))
    {
      cmp = *reverse * comp_self.compare_top(doc, scorer, comp_arg)?;
      if cmp != 0 {
        return Ok(cmp);
      }
    }

    Ok(0)
  }

  pub(crate) fn copy<S>(
    &mut self,
    slot: usize,
    doc: i32,
    scorer: &mut S,
    comparators: &mut [FieldComparatorEnum],
  ) -> Result<()>
  where
    S: Scorable + ?Sized,
  {
    debug_assert_eq!(
      self.comparators.len() + 1,
      comparators.len(),
      "comparators length mismatch"
    );

    self
      .first_comparator
      .copy(slot, doc, scorer, &mut comparators[0])?;
    for ((comp_self, _), comp_arg) in self
      .comparators
      .iter_mut()
      .zip(comparators.iter_mut().skip(1))
    {
      comp_self.copy(slot, doc, scorer, comp_arg)?;
    }

    Ok(())
  }

  pub(crate) fn set_scorer<S>(
    &mut self,
    scorer: &mut S,
    comparators: &mut [FieldComparatorEnum],
  ) -> Result<()>
  where
    S: Scorable + ?Sized,
  {
    debug_assert_eq!(
      self.comparators.len() + 1,
      comparators.len(),
      "comparators length mismatch"
    );

    self
      .first_comparator
      .set_scorer(scorer, &mut comparators[0])?;
    for ((comp_self, _), comp_arg) in self
      .comparators
      .iter_mut()
      .zip(comparators.iter_mut().skip(1))
    {
      comp_self.set_scorer(scorer, comp_arg)?;
    }

    Ok(())
  }

  pub(crate) fn competitive_iterator(
    &mut self,
    comparators: &mut [FieldComparatorEnum],
  ) -> Result<Option<LeafFieldComparatorDocIdSetIteratorRef<'_, LR>>> {
    debug_assert!(!comparators.is_empty());
    self
      .first_comparator
      .competitive_iterator(&mut comparators[0])
  }

  pub(crate) fn set_hits_threshold_reached(
    &mut self,
    comparators: &mut [FieldComparatorEnum],
  ) -> Result<()> {
    // this is needed for skipping functionality that is only relevant for the 1st comparator
    self
      .first_comparator
      .set_hits_threshold_reached(&mut comparators[0])
  }
}
