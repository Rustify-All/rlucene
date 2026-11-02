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
use crate::core::search::field_comparator::{
  FieldComparator, FieldComparatorEnum, FieldComparatorValue,
};
use crate::core::search::field_doc::{FieldDoc, FieldsValue};
use crate::core::search::leaf_field_comparator::LeafFieldComparatorEnum;
use crate::core::search::pruning::Pruning;
use crate::core::search::score_doc::{ScoreDoc, ScoreDocLike};
use crate::core::search::sort_field::SortFiledBase;
use crate::core::search::sort_field_enum::SortFieldEnum;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::priority_queue::{Compare, CompareEnum2, PriorityQueue};
use crate::impl_from_for_enum;
use std::fmt;
use std::fmt::{Display, Formatter};

/// A hit queue for sorting by hits by terms in more than one field
pub struct FieldValueHitQueue;
pub fn new(
  fields: &[SortFieldEnum],
  size: usize,
) -> Result<PriorityQueue<Entry, FieldValueHitQueueComparator>> {
  let num_comparators = fields.len();
  let mut comparators = Vec::with_capacity(num_comparators);
  let mut reverse_mul = Vec::with_capacity(num_comparators);

  for (i, field) in fields.iter().enumerate() {
    let reverse = if field.get_reverse() { -1 } else { 1 };
    reverse_mul.push(reverse);

    let pruning = if i == 0 {
      if num_comparators > 1 {
        Pruning::GreaterThan
      } else {
        Pruning::GreaterThanOrEqualTo
      }
    } else {
      Pruning::None
    };

    let comparator = field.get_comparator(size, pruning)?;
    comparators.push(comparator);
  }
  let comparator = if num_comparators == 1 {
    CompareEnum2::A(OneComparatorComparator::new(comparators, reverse_mul))
  } else {
    CompareEnum2::B(MultiComparatorsComparator::new(comparators, reverse_mul))
  };
  PriorityQueue::new(size, comparator)
}
/// Creates a hit queue sorted by the given list of fields.
///
/// **NOTE:**
/// The instances returned by this method pre-allocate a full array of length `num_hits`.
///
/// # Arguments
///
/// * `fields` – SortField array we are sorting by in priority order (highest priority first);
///   cannot be empty.
/// * `size` – The number of hits to retain.
///   Must be greater than zero.
pub fn create(
  fields: &[SortFieldEnum],
  size: usize,
) -> Result<PriorityQueue<Entry, FieldValueHitQueueComparator>> {
  if fields.is_empty() {
    return Err(LuceneError::illegal_argument(
      "Sort must contain at least one field",
    ));
  }
  new(fields, size)
}

#[derive(Clone, Default, Debug)]
pub struct Entry {
  pub base: ScoreDoc,
  pub slot: usize,
}
impl Entry {
  pub fn new(slot: usize, doc: i32) -> Self {
    let base = ScoreDoc::new(doc, f32::NAN);
    Self { base, slot }
  }
}
impl ScoreDocLike for Entry {
  fn doc(&self) -> i32 {
    self.base.doc
  }

  fn score(&self) -> f32 {
    self.base.score
  }

  fn shard_index(&self) -> i32 {
    self.base.shard_index
  }

  fn set_shard_index(&mut self, shard_index: i32) {
    self.base.shard_index = shard_index
  }

  fn set_score(&mut self, score: f32) {
    self.base.score = score
  }
}
impl fmt::Display for Entry {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "slot:{} {}", self.slot, self.base)
  }
}

/// An implementation of FieldValueHitQueue which is optimized in case there is just one comparator.
pub struct OneComparatorComparator {
  one_comparator: Box<[FieldComparatorEnum]>,
  one_reverse_mul: Box<[i32]>,
}
impl OneComparatorComparator {
  pub fn new(one_comparator: Vec<FieldComparatorEnum>, one_reverse_mul: Vec<i32>) -> Self {
    debug_assert_eq!(one_comparator.len(), one_reverse_mul.len());
    debug_assert!(one_comparator.len() == 1);
    Self {
      one_comparator: one_comparator.into_boxed_slice(),
      one_reverse_mul: one_reverse_mul.into_boxed_slice(),
    }
  }
}
impl Compare<Entry> for OneComparatorComparator {
  fn less_than(&self, hit_a: &Entry, hit_b: &Entry) -> Result<bool> {
    debug_assert!(self.one_comparator.len() == self.one_reverse_mul.len());
    debug_assert_ne!(hit_a.slot, hit_b.slot);

    let cmp_result = self.one_comparator[0].compare(hit_a.slot, hit_b.slot);
    let c = self.one_reverse_mul[0] * cmp_result;

    if c != 0 {
      Ok(c > 0)
    } else {
      Ok(hit_a.base.doc > hit_b.base.doc)
    }
  }
}
/// An implementation of FieldValueHitQueue which is optimized in case there is more than one comparator.
pub struct MultiComparatorsComparator {
  comparators: Box<[FieldComparatorEnum]>,
  reverse_mul: Box<[i32]>,
}

impl MultiComparatorsComparator {
  pub fn new(comparators: Vec<FieldComparatorEnum>, reverse_mul: Vec<i32>) -> Self {
    debug_assert_eq!(
      comparators.len(),
      reverse_mul.len(),
      "comparators and reverse_mul length must match"
    );
    Self {
      comparators: comparators.into_boxed_slice(),
      reverse_mul: reverse_mul.into_boxed_slice(),
    }
  }
}

impl Compare<Entry> for MultiComparatorsComparator {
  fn less_than(&self, hit_a: &Entry, hit_b: &Entry) -> Result<bool> {
    debug_assert_eq!(
      self.comparators.len(),
      self.reverse_mul.len(),
      "comparators/reverse_mul length mismatch"
    );
    debug_assert_ne!(hit_a.slot, hit_b.slot);

    let num_comparators = self.comparators.len();

    for i in 0..num_comparators {
      let cmp_result = self.comparators[i].compare(hit_a.slot, hit_b.slot);
      let c = self.reverse_mul[i] * cmp_result;

      if c != 0 {
        return Ok(c > 0);
      }
    }
    Ok(hit_a.base.doc > hit_b.base.doc)
  }
}
pub type FieldValueHitQueueComparator =
  CompareEnum2<OneComparatorComparator, MultiComparatorsComparator>;

impl PriorityQueue<Entry, FieldValueHitQueueComparator> {
  pub fn get_leaf_comparator<LR>(
    &mut self,
    context: &LeafReaderContext<LR>,
  ) -> Result<Vec<LeafFieldComparatorEnum<LR>>>
  where
    LR: LeafReader,
  {
    match &mut self.compare {
      CompareEnum2::A(one_comp) => {
        debug_assert!(one_comp.one_comparator.len() == 1);
        let comp = &mut one_comp.one_comparator[0];
        Ok(vec![comp.get_leaf_comparator(context)?])
      },
      CompareEnum2::B(multi_comp) => {
        let mut v = Vec::with_capacity(multi_comp.comparators.len());
        for x in &mut multi_comp.comparators {
          v.push(x.get_leaf_comparator(context)?);
        }
        Ok(v)
      },
    }
  }
  pub fn get_reverse_mul(&self) -> &[i32] {
    match &self.compare {
      CompareEnum2::A(one_comp) => &one_comp.one_reverse_mul,
      CompareEnum2::B(multi_comp) => &multi_comp.reverse_mul,
    }
  }
  pub fn get_comparators(&self) -> &[FieldComparatorEnum] {
    match &self.compare {
      CompareEnum2::A(one_comp) => &one_comp.one_comparator,
      CompareEnum2::B(multi_comp) => &multi_comp.comparators,
    }
  }
  pub fn get_comparators_mut(&mut self) -> &mut [FieldComparatorEnum] {
    match &mut self.compare {
      CompareEnum2::A(one_comp) => &mut one_comp.one_comparator,
      CompareEnum2::B(multi_comp) => &mut multi_comp.comparators,
    }
  }
  pub(crate) fn get_comparators_and_reverse_mul(&mut self) -> (&mut [FieldComparatorEnum], &[i32]) {
    match &mut self.compare {
      CompareEnum2::A(one_comp) => (&mut one_comp.one_comparator, &one_comp.one_reverse_mul),
      CompareEnum2::B(multi_comp) => (&mut multi_comp.comparators, &multi_comp.reverse_mul),
    }
  }
  /// Given a queue [`Entry`], creates a corresponding [`FieldDoc`] that contains the values used to sort the
  /// given document. These values are not the raw values out of the index, but the internal
  /// representation of them. This allows the search hit to be collated with hits from other
  /// searchers.
  ///
  /// # Arguments
  ///
  /// * `entry` – The [`Entry`] used to create a [`FieldDoc`].
  ///
  /// # Returns
  ///
  /// The newly created [`FieldDoc`].
  pub(crate) fn fill_fields(&self, entry: Entry) -> FieldDoc {
    let comparators = self.get_comparators();
    let n = comparators.len();
    let mut fields = Vec::with_capacity(n);
    for comp in comparators {
      let value = comp
        .value(entry.slot)
        .unwrap_or_else(FieldComparatorValue::missing);
      fields.push(value);
    }
    FieldDoc::with_fields(entry.base.doc, entry.base.score, fields)
  }
}

#[derive(Clone, Debug)]
pub enum TopFieldScoreDoc {
  Entry(Entry),
  Field(FieldDoc),
  Score(ScoreDoc),
}
impl TopFieldScoreDoc {
  pub fn as_entry(&self) -> Option<&Entry> {
    match self {
      TopFieldScoreDoc::Entry(v) => Some(v),
      _ => None,
    }
  }

  pub fn as_field(&self) -> Option<&FieldDoc> {
    match self {
      TopFieldScoreDoc::Field(v) => Some(v),
      _ => None,
    }
  }

  pub fn as_score(&self) -> Option<&ScoreDoc> {
    match self {
      TopFieldScoreDoc::Score(v) => Some(v),
      _ => None,
    }
  }
  pub fn into_entry(self) -> Option<Entry> {
    match self {
      TopFieldScoreDoc::Entry(v) => Some(v),
      _ => None,
    }
  }

  pub fn into_field(self) -> Option<FieldDoc> {
    match self {
      TopFieldScoreDoc::Field(v) => Some(v),
      _ => None,
    }
  }

  pub fn into_score(self) -> Option<ScoreDoc> {
    match self {
      TopFieldScoreDoc::Score(v) => Some(v),
      _ => None,
    }
  }
  pub fn fields(&self) -> Result<&[FieldsValue]> {
    match self {
      TopFieldScoreDoc::Field(fd) => Ok(fd.fields.as_slice()),
      _ => Err(LuceneError::illegal_state("not a FieldDoc variant")),
    }
  }
  pub fn slot(&self) -> Result<usize> {
    match self {
      TopFieldScoreDoc::Entry(e) => Ok(e.slot),
      _ => Err(LuceneError::illegal_state(
        "other variants do not have slot",
      )),
    }
  }
  pub fn doc(&self) -> i32 {
    match self {
      TopFieldScoreDoc::Entry(e) => e.doc(),
      TopFieldScoreDoc::Field(fd) => fd.doc(),
      TopFieldScoreDoc::Score(sd) => sd.doc(),
    }
  }
  pub fn score_doc_mut(&mut self) -> &mut ScoreDoc {
    match self {
      TopFieldScoreDoc::Entry(e) => &mut e.base,
      TopFieldScoreDoc::Field(fd) => &mut fd.base,
      TopFieldScoreDoc::Score(sd) => sd,
    }
  }
  pub fn score_doc(&self) -> &ScoreDoc {
    match self {
      TopFieldScoreDoc::Entry(e) => &e.base,
      TopFieldScoreDoc::Field(fd) => &fd.base,
      TopFieldScoreDoc::Score(sd) => sd,
    }
  }
}

impl Display for TopFieldScoreDoc {
  fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
    match self {
      TopFieldScoreDoc::Entry(e) => write!(f, "Entry: {}", e),
      TopFieldScoreDoc::Field(fd) => write!(f, "FieldDoc: {}", fd),
      TopFieldScoreDoc::Score(sd) => write!(f, "ScoreDoc: {}", sd),
    }
  }
}

impl Default for TopFieldScoreDoc {
  fn default() -> Self {
    TopFieldScoreDoc::Entry(Entry::default())
  }
}

impl ScoreDocLike for TopFieldScoreDoc {
  fn doc(&self) -> i32 {
    match self {
      TopFieldScoreDoc::Entry(e) => e.doc(),
      TopFieldScoreDoc::Field(fd) => fd.doc(),
      TopFieldScoreDoc::Score(sd) => sd.doc(),
    }
  }

  fn score(&self) -> f32 {
    match self {
      TopFieldScoreDoc::Entry(e) => e.score(),
      TopFieldScoreDoc::Field(fd) => fd.score(),
      TopFieldScoreDoc::Score(sd) => sd.score(),
    }
  }

  fn shard_index(&self) -> i32 {
    match self {
      TopFieldScoreDoc::Entry(e) => e.shard_index(),
      TopFieldScoreDoc::Field(fd) => fd.shard_index(),
      TopFieldScoreDoc::Score(sd) => sd.shard_index(),
    }
  }

  fn set_shard_index(&mut self, shard_index: i32) {
    match self {
      TopFieldScoreDoc::Entry(e) => e.set_shard_index(shard_index),
      TopFieldScoreDoc::Field(fd) => fd.set_shard_index(shard_index),
      TopFieldScoreDoc::Score(sd) => sd.set_shard_index(shard_index),
    }
  }

  fn set_score(&mut self, score: f32) {
    match self {
      TopFieldScoreDoc::Entry(e) => e.set_score(score),
      TopFieldScoreDoc::Field(fd) => fd.set_score(score),
      TopFieldScoreDoc::Score(sd) => sd.set_score(score),
    }
  }
}
impl_from_for_enum!(
    TopFieldScoreDoc,
    Entry => Entry,
    FieldDoc => Field,
    ScoreDoc => Score
);
