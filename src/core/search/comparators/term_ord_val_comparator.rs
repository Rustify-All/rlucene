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
use crate::core::codecs::block_term_state::TermStateEnum;
use crate::core::index::BytesRefValueEnum;
use crate::core::index::doc_values::{DocValues, Sorted, SortedSet};
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::leaf_reader::{LRPosting, LRTermsEnum, LeafReader};
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::postings_enum::NONE;
use crate::core::index::sorted_doc_values::SortedDocValues;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::{SeekStatus, TermsEnum};
use crate::core::index::{BytesRef, BytesRefValue};
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::field_comparator::{FieldComparator, TermTopValue};
use crate::core::search::index_searcher::get_max_clause_count;
use crate::core::search::leaf_field_comparator::LeafFieldComparator;
use crate::core::search::pruning::Pruning;
use crate::core::search::scorable::Scorable;
use crate::core::search::sorted_set_selector::SortedDocValuesWrap;
use crate::core::util::ToInt;
use crate::core::util::access::ByteSource;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::priority_queue::{Compare, PriorityQueue};
use std::borrow::Cow;
use std::collections::VecDeque;

/// Sorts by field's natural Term sort order, using ordinals.
///
/// This is functionally equivalent to
/// [`TermValComparator`](crate::core::search::field_comparator::TermValComparator),
/// but it first resolves the string to their relative ordinal positions
/// (using the index returned by
/// [`LeafReader::get_sorted_doc_values`]),
/// and does most comparisons using the ordinals.
///
/// For medium to large results, this comparator will be much faster than
/// [`TermValComparator`](crate::core::search::field_comparator::TermValComparator).
/// For very small result sets it may be slower.
pub struct TermOrdValComparator {
  /// Ords for each slot.
  pub(crate) ords: Box<[i32]>,
  /// Values for each slot.
  pub(crate) values: Box<[Option<BytesRef<Vec<u8>>>]>,
  /// Which reader last copied a value into the slot. When
  ///   we compare two slots, we just compare-by-ord if the
  ///  readerGen is the same; else we must compare the
  ///  values (slower).
  pub(crate) reader_gen: Box<[i32]>,
  /// Gen of current reader we are on.
  pub(crate) current_reader_gen: i32,
  pub(crate) field: String,
  reverse: bool,
  sort_missing_last: bool,
  /// Bottom value (same as `values[bottomSlot]` once bottomSlot is set).  Cached for faster compares.
  pub(crate) bottom_value: Option<usize>,
  /// Bottom slot, or None if the queue isn't full yet.
  pub(crate) bottom_slot: Option<usize>,
  /// Set by `set_top_value`.
  pub(crate) top_value: Option<TermTopValue>,
  /// -1 if missing values are sorted first, 1 if they are sorted last
  pub(crate) missing_sort_cmp: i32,
  /// Whether this is the only comparator.
  single_sort: bool,
  /// Whether this comparator is allowed to skip documents.
  can_skip_documents: bool,
  /// Whether the collector is done with counting hits so that we can start skipping documents.
  hits_threshold_reached: bool,
}
impl TermOrdValComparator {
  pub fn new<FName>(
    field: FName,
    num_hits: usize,
    sort_missing_last: bool,
    reverse: bool,
    pruning: Pruning,
  ) -> Self
  where
    FName: Into<String>,
  {
    let field = field.into();
    let can_skip_documents = pruning != Pruning::None;
    Self {
      ords: vec![0; num_hits].into_boxed_slice(),
      values: vec![None; num_hits].into_boxed_slice(),
      reader_gen: vec![0; num_hits].into_boxed_slice(),
      current_reader_gen: -1,
      field,
      reverse,
      sort_missing_last,
      bottom_value: None,
      bottom_slot: None,
      top_value: None,
      missing_sort_cmp: if sort_missing_last { 1 } else { -1 },
      single_sort: false,
      can_skip_documents,
      hits_threshold_reached: false,
    }
  }
}
impl FieldComparator for TermOrdValComparator {
  type V = BytesRef<Vec<u8>>;

  fn compare(&self, slot1: usize, slot2: usize) -> i32 {
    if self.reader_gen[slot1] == self.reader_gen[slot2] {
      return self.ords[slot1] - self.ords[slot2];
    }

    let val1 = self.values[slot1].as_ref();
    let val2 = self.values[slot2].as_ref();

    match (val1, val2) {
      (None, None) => 0,
      (None, Some(_)) => self.missing_sort_cmp,
      (Some(_), None) => -self.missing_sort_cmp,
      (Some(v1), Some(v2)) => v1.cmp(v2).to_int(),
    }
  }

  fn set_top_value(&mut self, value: Self::V) -> Result<()> {
    // None is fine: it means the last doc of the prior
    // search was missing this value
    self.top_value = Some(TermTopValue::Owned(value));
    Ok(())
  }

  fn value(&self, slot: usize) -> Option<Self::V> {
    // TODO IMPORTANT: avoid the clone here
    self.values[slot].clone()
  }

  type LeafFieldComparator<LR>
    = TermOrdValLeafComparator<LR>
  where
    LR: LeafReader;

  fn get_leaf_comparator<LR>(
    &mut self,
    context: &LeafReaderContext<LR>,
  ) -> Result<Self::LeafFieldComparator<LR>>
  where
    LR: LeafReader,
  {
    self.current_reader_gen += 1;
    let c = |context: &LeafReaderContext<LR>, field: &str| -> Result<TermOrdValDocValues<LR>> {
      Ok(TermOrdValDocValues::B(get_sorted_doc_values(
        context, field,
      )?))
    };
    TermOrdValLeafComparator::new(context, c, self)
  }

  fn compare_values(&self, val1: Option<&Self::V>, val2: Option<&Self::V>) -> Result<i32> {
    Ok(match (val1, val2) {
      (None, None) => 0,
      (None, Some(_)) => self.missing_sort_cmp,
      (Some(_), None) => -self.missing_sort_cmp,
      (Some(v1), Some(v2)) => v1.cmp(v2).to_int(),
    })
  }

  fn set_single_sort(&mut self) {
    self.single_sort = true;
  }

  fn disable_skipping(&mut self) {
    self.can_skip_documents = false;
  }
}
/// Retrieves the SortedDocValues for the field in this segment
fn get_sorted_doc_values<LR>(context: &LeafReaderContext<LR>, field: &str) -> Result<Sorted<LR>>
where
  LR: LeafReader,
{
  DocValues::get_sorted(context.reader(), field)
}
pub struct TermOrdValLeafComparator<LR>
where
  LR: LeafReader,
{
  /// Current reader's doc ord/values.
  pub(crate) terms_index: TermOrdValDocValues<LR>,
  /// True if current bottom slot matches the current reader.
  pub(crate) bottom_same_reader: bool,
  /// Bottom ord (same as ords[bottomSlot] once bottomSlot is set).  Cached for faster compares.
  pub(crate) bottom_ord: i32,
  pub(crate) top_same_reader: bool,
  pub(crate) top_ord: i32,
  /// Which ordinal to use for a missing value.
  pub(crate) missing_ord: i32,
  competitive_iterator: Option<TermOrdValCompetitiveIterator<LR>>,
  dense: bool,
}
impl<LR> TermOrdValLeafComparator<LR>
where
  LR: LeafReader,
{
  pub fn new<F>(
    context: &LeafReaderContext<LR>,
    doc_value_producer: F,
    comparator: &mut TermOrdValComparator,
  ) -> Result<Self>
  where
    F: Fn(&LeafReaderContext<LR>, &str) -> Result<TermOrdValDocValues<LR>>,
  {
    let mut terms_index = doc_value_producer(context, &comparator.field)?;
    let missing_ord = if comparator.sort_missing_last {
      i32::MAX
    } else {
      -1
    };
    let (top_ord, top_same_reader) = if let Some(ref top_value) = comparator.top_value {
      // Recompute topOrd/SameReader
      let ord = terms_index.lookup_term(top_value.as_ref()?)?;
      if ord >= 0 {
        (ord, true)
      } else {
        (-ord - 2, false)
      }
    } else {
      (missing_ord, true)
    };

    let mut leaf = TermOrdValLeafComparator {
      terms_index,
      bottom_same_reader: false,
      bottom_ord: -1,
      top_same_reader,
      top_ord,
      missing_ord,
      competitive_iterator: None,
      dense: false,
    };

    if let Some(bottom_slot) = comparator.bottom_slot {
      leaf.set_bottom(bottom_slot, comparator)?;
    }

    let enable_skipping = if !comparator.can_skip_documents {
      leaf.dense = false;
      false
    } else {
      let field_infos = context.reader().get_field_infos()?;
      let field_info = field_infos.field_info_by_name(&comparator.field)?;
      match field_info {
        None => {
          if leaf.terms_index.get_value_count()? != 0 {
            return Err(LuceneError::illegal_state(format!(
              "Field [{}] cannot be found in field infos",
              comparator.field
            )));
          }
          leaf.dense = false;
          true
        },

        Some(info) if *info.get_index_options() == IndexOptions::None => {
          // No terms index
          leaf.dense = false;
          false
        },

        Some(_) => {
          let terms = context.reader().terms(&comparator.field)?;
          leaf.dense = match terms {
            None => false,
            Some(ref t) => t.get_sum_doc_freq()? == context.reader().max_doc()? as i64,
          };

          if leaf.dense || comparator.top_value.is_some() {
            true
          } else if comparator.reverse == comparator.sort_missing_last {
            // Missing values are always competitive, we can never skip
            false
          } else {
            true
          }
        },
      }
    };

    if enable_skipping {
      let terms = context.reader().terms(&comparator.field)?;
      if let Some(terms) = terms {
        let t = terms.iterator()?;
        let docs_with_field = if leaf.dense {
          None
        } else {
          Some(doc_value_producer(context, &comparator.field)?)
        };

        leaf.competitive_iterator = Some(TermOrdValCompetitiveIterator::new(
          context,
          leaf.dense,
          docs_with_field,
          t,
        )?);
      }
    }
    leaf.update_competitive_iterator(comparator)?;

    Ok(leaf)
  }
  fn update_competitive_iterator(&mut self, comparator: &TermOrdValComparator) -> Result<()> {
    if self.competitive_iterator.is_none()
      || !comparator.hits_threshold_reached
      || comparator.bottom_slot.is_none()
    {
      return Ok(());
    }
    // This logic to figure out min and max ords is quite complex and verbose, can it be made
    // simpler?
    let min_ord: i32;
    let max_ord: i32;

    if !comparator.reverse {
      if let Some(ref _top_value) = comparator.top_value {
        if self.top_same_reader {
          min_ord = self.top_ord;
        } else {
          // In the case when the top value doesn't exist in the segment, topOrd is set as the
          // previous ord, and we are only interested in values that compare strictly greater than
          // this.
          min_ord = self.top_ord + 1;
        }
      } else if comparator.sort_missing_last || self.dense {
        min_ord = 0;
      } else {
        // Missing values are still competitive.
        min_ord = -1;
      }

      if self.bottom_ord == self.missing_ord {
        // The queue still contains missing values.
        if comparator.single_sort {
          // If there is no tie breaker, we can start ignoring missing values from now on.
          max_ord = self.terms_index.get_value_count()? - 1;
        } else {
          max_ord = i32::MAX;
        }
      } else if self.bottom_same_reader {
        // If there is no tie breaker, we can start ignoring values that compare equal to the
        // current top value too.
        max_ord = if comparator.single_sort {
          self.bottom_ord - 1
        } else {
          self.bottom_ord
        };
      } else {
        max_ord = self.bottom_ord;
      }
    } else {
      if self.bottom_ord == self.missing_ord {
        // The queue still contains missing values.
        if comparator.single_sort {
          // If there is no tie breaker, we can start ignoring missing values from now on.
          min_ord = 0;
        } else {
          min_ord = -1;
        }
      } else if self.bottom_same_reader {
        // If there is no tie breaker, we can start ignoring values that compare equal to the
        // current top value too.
        min_ord = if comparator.single_sort {
          self.bottom_ord + 1
        } else {
          self.bottom_ord
        };
      } else {
        min_ord = self.bottom_ord + 1;
      }

      if comparator.top_value.is_some() {
        max_ord = self.top_ord;
      } else if !comparator.sort_missing_last || self.dense {
        max_ord = self.terms_index.get_value_count()? - 1;
      } else {
        max_ord = i32::MAX;
      }
    }

    if min_ord == -1 || max_ord == i32::MAX {
      // Missing values are still competitive, we can't skip yet.
      return Ok(());
    }

    debug_assert!(min_ord >= 0);
    debug_assert!(max_ord < self.terms_index.get_value_count()?);

    let competitive_iterator = self
      .competitive_iterator
      .as_mut()
      .ok_or_else(|| LuceneError::illegal_state("competitive iterator is not initialized"))?;
    competitive_iterator.update(&mut self.terms_index, min_ord, max_ord)?;

    Ok(())
  }
  fn get_ord_for_doc(&mut self, doc: i32) -> Result<i32> {
    if self.terms_index.advance_exact(doc)? {
      Ok(self.terms_index.ord_value()?)
    } else {
      Ok(-1)
    }
  }
}
impl<LR> LeafFieldComparator for TermOrdValLeafComparator<LR>
where
  LR: LeafReader,
{
  type FieldComparator = TermOrdValComparator;
  fn set_bottom(
    &mut self,
    bottom_slot: usize,
    comparator: &mut Self::FieldComparator,
  ) -> Result<()> {
    comparator.bottom_slot = Some(bottom_slot);
    comparator.bottom_value = Some(bottom_slot);

    if comparator.current_reader_gen == comparator.reader_gen[bottom_slot] {
      self.bottom_ord = comparator.ords[bottom_slot];
      self.bottom_same_reader = true;
    } else {
      let has_value = comparator.values[bottom_slot].is_some();
      match has_value {
        false => {
          // missingOrd is None for all segments
          debug_assert!(comparator.ords[bottom_slot] == self.missing_ord);
          self.bottom_ord = self.missing_ord;
          self.bottom_same_reader = true;
          comparator.reader_gen[bottom_slot] = comparator.current_reader_gen;
        },
        true => {
          let target = match comparator.values[bottom_slot].as_ref() {
            None => {
              return Err(LuceneError::illegal_state(
                "bottomValue is None but ords[bottomSlot] is not missingOrd",
              ));
            },
            Some(v) => v,
          };
          let ord = self.terms_index.lookup_term(target)?;
          if ord < 0 {
            self.bottom_ord = -ord - 2;
            self.bottom_same_reader = false;
          } else {
            self.bottom_ord = ord;
            self.bottom_same_reader = true;
            comparator.reader_gen[bottom_slot] = comparator.current_reader_gen;
            comparator.ords[bottom_slot] = self.bottom_ord;
          }
        },
      }
    }

    self.update_competitive_iterator(comparator)?;
    Ok(())
  }

  fn compare_bottom<S>(
    &mut self,
    doc: i32,
    _scorer: &mut S,
    comparator: &mut Self::FieldComparator,
  ) -> Result<i32>
  where
    S: Scorable + ?Sized,
  {
    debug_assert!(comparator.bottom_slot.is_some());

    let mut doc_ord = self.get_ord_for_doc(doc)?;
    if doc_ord == -1 {
      doc_ord = self.missing_ord;
    }

    if self.bottom_same_reader {
      // ord is precisely comparable, even in the equal case
      Ok(self.bottom_ord - doc_ord)
    } else if self.bottom_ord >= doc_ord {
      // the equals case always means bottom is > doc
      // (because we set bottomOrd to the lower bound in setBottom):
      Ok(1)
    } else {
      Ok(-1)
    }
  }

  fn compare_top<S>(
    &mut self,
    doc: i32,
    _scorer: &mut S,
    _comparator: &mut Self::FieldComparator,
  ) -> Result<i32>
  where
    S: Scorable + ?Sized,
  {
    let mut ord = self.get_ord_for_doc(doc)?;
    if ord == -1 {
      ord = self.missing_ord;
    }

    if self.top_same_reader {
      // ord is precisely comparable, even in the equal case
      Ok(self.top_ord - ord)
    } else if ord <= self.top_ord {
      // the equals case always means doc is < value
      // (because we set topOrd to the lower bound)
      Ok(1)
    } else {
      Ok(-1)
    }
  }

  fn copy<S>(
    &mut self,
    slot: usize,
    doc: i32,
    _scorer: &mut S,
    comparator: &mut Self::FieldComparator,
  ) -> Result<()>
  where
    S: Scorable + ?Sized,
  {
    let mut ord = self.get_ord_for_doc(doc)?;
    if ord == -1 {
      ord = self.missing_ord;
      comparator.values[slot] = None;
    } else {
      debug_assert!(ord >= 0);

      let value = self.terms_index.lookup_ord(ord)?;
      match (&value, comparator.values[slot].as_mut()) {
        (
          BytesRefValueEnum::Buffer(Cow::Borrowed(_)) | BytesRefValueEnum::Slice(_),
          Some(buffer),
        ) => {
          buffer.copy_from_slice(value.as_bytes());
        },
        (_, _) => comparator.values[slot] = Some(value.into_owned()),
      }
    }

    comparator.ords[slot] = ord;
    comparator.reader_gen[slot] = comparator.current_reader_gen;

    Ok(())
  }

  fn set_scorer<S>(
    &mut self,
    _scorer: &mut S,
    _comparator: &mut Self::FieldComparator,
  ) -> Result<()>
  where
    S: Scorable + ?Sized,
  {
    Ok(())
  }

  type DocIdSetIteratorRef<'a>
    = &'a mut TermOrdValCompetitiveIterator<LR>
  where
    LR: 'a;

  fn competitive_iterator(
    &mut self,
    _comparator: &mut Self::FieldComparator,
  ) -> Result<Option<Self::DocIdSetIteratorRef<'_>>> {
    Ok(self.competitive_iterator.as_mut())
  }

  fn set_hits_threshold_reached(&mut self, comparator: &mut Self::FieldComparator) -> Result<()> {
    comparator.hits_threshold_reached = true;
    self.update_competitive_iterator(comparator)
  }
}

const MAX_TERMS: usize = 1024;
pub struct TermOrdValCompetitiveIterator<LR>
where
  LR: LeafReader,
{
  max_doc: i32,
  dense: bool,
  doc: i32,
  postings: VecDeque<i32>,
  postings_init: bool,
  terms: LRTermsEnum<LR>,
  docs_with_field: Option<TermOrdValDocValues<LR>>,
  // if docs_with_field is active, dense must be false
  using_skip: bool,
  disjunction: Option<PriorityQueue<PostingsEnumAndOrd<LRPosting<LR>>, PostingsEnumAndOrdCmp>>,
}
impl<LR> TermOrdValCompetitiveIterator<LR>
where
  LR: LeafReader,
{
  pub fn new(
    reader: &LeafReaderContext<LR>,
    dense: bool,
    docs_with_field: Option<TermOrdValDocValues<LR>>,
    terms_enum: LRTermsEnum<LR>,
  ) -> Result<Self> {
    let max_doc = reader.reader().max_doc()?;
    debug_assert!(
      !(dense && docs_with_field.is_some()),
      "docs_with_field must be None when dense = true"
    );
    Ok(Self {
      max_doc,
      dense,
      doc: -1,
      postings: VecDeque::new(),
      postings_init: false,
      terms: terms_enum,
      docs_with_field,
      using_skip: false,
      disjunction: None,
    })
  }
  /// Update this iterator to only match postings whose term has an ordinal between `minOrd` included and `maxOrd` included.
  fn update(
    &mut self,
    doc_values: &mut TermOrdValDocValues<LR>,
    min_ord: i32,
    max_ord: i32,
  ) -> Result<()> {
    let max_terms = std::cmp::min(MAX_TERMS, get_max_clause_count());
    let size = std::cmp::max(0, max_ord - min_ord + 1) as usize;

    if size > max_terms {
      // Dense fields do not have a docs-with-field iterator to use for skipping.
      self.using_skip = !self.dense;
    } else if !self.postings_init {
      self.init(doc_values, min_ord, max_ord)?;
    } else if size < self.postings.len() {
      // One or more ords got removed
      debug_assert!(self.postings.front().is_none_or(|ord| *ord <= min_ord));
      while self.postings.front().is_some_and(|ord| *ord < min_ord) {
        self.postings.pop_front();
      }

      debug_assert!(self.postings.back().is_none_or(|ord| *ord >= max_ord));
      while self.postings.back().is_some_and(|ord| *ord > max_ord) {
        self.postings.pop_back();
      }
      let disjunction = self
        .disjunction
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("postings disjunction is not initialized"))?;
      let iterms = disjunction.take_heap_array();
      // debug_assert!(
      //   iterms.len() == self.postings.len(),
      //   "priority queue size must match postings size"
      // );
      let (min_ord, max_ord) = match (self.postings.front(), self.postings.back()) {
        (Some(min_ord), Some(max_ord)) => (*min_ord, *max_ord),
        _ => (0, 0),
      };
      for v in iterms {
        if v.ord < min_ord || v.ord > max_ord {
          // this ord was removed
          continue;
        }
        disjunction.add(v)?;
      }
    }

    Ok(())
  }
  /// For the first time, this iterator is allowed to skip documents.
  /// It needs to pull [`PostingsEnum`](crate::core::index::postings_enum::PostingsEnum)s from the terms dictionary of the inverted index
  /// and create a priority queue out of them.
  fn init(
    &mut self,
    doc_values: &mut TermOrdValDocValues<LR>,
    min_ord: i32,
    max_ord: i32,
  ) -> Result<()> {
    self.postings_init = true;
    let size = std::cmp::max(0, max_ord - min_ord + 1) as usize;
    self.postings = VecDeque::with_capacity(size);

    debug_assert!(self.disjunction.is_none());
    let mut disjunction = PriorityQueue::new(size, PostingsEnumAndOrdCmp)?;
    if size > 0 {
      let min_term = doc_values.lookup_ord(min_ord)?.into_value().into_cow();
      if !self.terms.seek_exact(&min_term)? {
        return Err(LuceneError::illegal_state(format!(
          "Term {} exists in doc values but not in the terms index",
          min_term
        )));
      }

      disjunction.add(PostingsEnumAndOrd::new(
        self.terms.postings_with_flags(None, NONE as i32)?,
        min_ord,
      ))?;
      self.postings.push_back(min_ord);

      for ord in (min_ord + 1)..=max_ord {
        let expected_ord = {
          let next = self.terms.next()?;
          let next = match next {
            Some(term) => term,
            None => {
              return Err(LuceneError::illegal_state(format!(
                "Terms have more than {ord} unique terms while doc values have exactly {ord} terms"
              )));
            },
          };
          doc_values.lookup_term(&next.as_bytes_ref())?
        };
        debug_assert!(
          expected_ord == ord,
          "docValuesTerms not aligned with terms index"
        );
        disjunction.add(PostingsEnumAndOrd::new(
          self.terms.postings_with_flags(None, NONE as i32)?,
          ord,
        ))?;
        self.postings.push_back(ord);
      }
    }
    self.disjunction = Some(disjunction);
    Ok(())
  }
}
impl<LR> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for TermOrdValCompetitiveIterator<LR>
where
  LR: LeafReader,
{
}
impl<LR> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for TermOrdValCompetitiveIterator<LR>
where
  LR: LeafReader,
{
}

impl<LR> DocIdSetIterator for TermOrdValCompetitiveIterator<LR>
where
  LR: LeafReader,
{
  fn doc_id(&self) -> i32 {
    self.doc
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.advance(self.doc_id() + 1)
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    if target >= self.max_doc {
      self.doc = NO_MORE_DOCS;
      return Ok(self.doc);
    }

    let Some(disjunction) = self.disjunction.as_mut() else {
      if self.using_skip {
        // The field is sparse and we're only interested in documents that have a value.
        debug_assert!(!self.dense);
        self.doc = self
          .docs_with_field
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("docs-with-field iterator is missing"))?
          .advance(target)?;
        return Ok(self.doc);
      } else {
        // We haven't started skipping yet
        self.doc = target;
        return Ok(self.doc);
      }
    };

    let Some(mut top) = disjunction.top_mut() else {
      // priority queue is empty, none of the remaining documents are competitive
      self.doc = NO_MORE_DOCS;
      return Ok(self.doc);
    };

    while top.postings.doc_id() < target {
      top.postings.advance(target)?;
      top = disjunction.update_top()?;
    }

    self.doc = top.postings.doc_id();
    Ok(self.doc)
  }

  fn cost(&self) -> Result<i64> {
    Ok(self.max_doc as i64)
  }
}
struct PostingsEnumAndOrd<PE> {
  postings: PE,
  ord: i32,
}
impl<PE> PostingsEnumAndOrd<PE> {
  pub fn new(postings: PE, ord: i32) -> Self {
    Self { postings, ord }
  }
}
struct PostingsEnumAndOrdCmp;
impl<PE> Compare<PostingsEnumAndOrd<PE>> for PostingsEnumAndOrdCmp
where
  PE: DocIdSetIterator,
{
  fn less_than(&self, a: &PostingsEnumAndOrd<PE>, b: &PostingsEnumAndOrd<PE>) -> Result<bool> {
    Ok(a.postings.doc_id() < b.postings.doc_id())
  }
}
pub enum TermOrdValDocValues<LR>
where
  LR: LeafReader,
{
  A(SortedDocValuesWrap<SortedSet<LR>>),
  B(Sorted<LR>),
}

impl<LR> DocValuesIterator for TermOrdValDocValues<LR>
where
  LR: LeafReader,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    match self {
      Self::A(values) => values.advance_exact(target),
      Self::B(values) => values.advance_exact(target),
    }
  }
}

impl<LR> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for TermOrdValDocValues<LR>
where
  LR: LeafReader,
{
}
impl<LR> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for TermOrdValDocValues<LR> where
  LR: LeafReader
{
}

impl<LR> DocIdSetIterator for TermOrdValDocValues<LR>
where
  LR: LeafReader,
{
  fn doc_id(&self) -> i32 {
    match self {
      Self::A(values) => values.doc_id(),
      Self::B(values) => values.doc_id(),
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    match self {
      Self::A(values) => values.next_doc(),
      Self::B(values) => values.next_doc(),
    }
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::A(values) => values.advance(target),
      Self::B(values) => values.advance(target),
    }
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::A(values) => values.slow_advance(target),
      Self::B(values) => values.slow_advance(target),
    }
  }

  fn cost(&self) -> Result<i64> {
    match self {
      Self::A(values) => values.cost(),
      Self::B(values) => values.cost(),
    }
  }
}

pub enum TermOrdValTermsEnum<'a, LR>
where
  LR: LeafReader,
  LR::SortedDocValues: 'a,
  LR::SortedSetDocValues: 'a,
{
  A(<SortedDocValuesWrap<SortedSet<LR>> as SortedDocValues>::TermsEnum<'a>),
  B(<Sorted<LR> as SortedDocValues>::TermsEnum<'a>),
}

impl<'a, LR> BytesRefIterator for TermOrdValTermsEnum<'a, LR>
where
  LR: LeafReader,
  LR::SortedDocValues: 'a,
  LR::SortedSetDocValues: 'a,
{
  type Value<'b>
    = BytesRefValueEnum<'b>
  where
    Self: 'b;

  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    match self {
      Self::A(terms) => terms
        .next()
        .map(|value| value.map(BytesRefValue::into_value)),
      Self::B(terms) => terms
        .next()
        .map(|value| value.map(BytesRefValue::into_value)),
    }
  }

  fn set_next(&mut self) -> Result<bool> {
    match self {
      Self::A(terms) => terms.set_next(),
      Self::B(terms) => terms.set_next(),
    }
  }
}

impl<'a, LR> TermsEnum for TermOrdValTermsEnum<'a, LR>
where
  LR: LeafReader,
  LR::SortedDocValues: 'a,
  LR::SortedSetDocValues: 'a,
{
  type AttributeSource<'b>
    = <<SortedDocValuesWrap<SortedSet<LR>> as SortedDocValues>::TermsEnum<'a> as TermsEnum>::AttributeSource<'b>
  where
    Self: 'b;
  type AttributeSourceMut<'b>
    = <<SortedDocValuesWrap<SortedSet<LR>> as SortedDocValues>::TermsEnum<'a> as TermsEnum>::AttributeSourceMut<'b>
  where
    Self: 'b;

  fn attributes(&self) -> Result<Self::AttributeSource<'_>> {
    match self {
      Self::A(terms) => terms.attributes(),
      Self::B(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn attributes_mut(&mut self) -> Result<Self::AttributeSourceMut<'_>> {
    match self {
      Self::A(terms) => terms.attributes_mut(),
      Self::B(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn seek_exact<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<bool> {
    match self {
      Self::A(terms) => terms.seek_exact(term),
      Self::B(terms) => terms.seek_exact(term),
    }
  }

  fn prepare_seek_exact<BS: ByteSource>(&mut self, text: &BytesRef<BS>) -> Result<Option<()>> {
    match self {
      Self::A(terms) => terms.prepare_seek_exact(text),
      Self::B(terms) => terms.prepare_seek_exact(text),
    }
  }

  fn get_prepare_seek_exact_status<BS: ByteSource>(
    &mut self,
    target: &BytesRef<BS>,
  ) -> Result<bool> {
    match self {
      Self::A(terms) => terms.get_prepare_seek_exact_status(target),
      Self::B(terms) => terms.get_prepare_seek_exact_status(target),
    }
  }

  fn seek_ceil<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<SeekStatus> {
    match self {
      Self::A(terms) => terms.seek_ceil(term),
      Self::B(terms) => terms.seek_ceil(term),
    }
  }

  fn seek_exact_with_ord(&mut self, ord: i64) -> Result<()> {
    match self {
      Self::A(terms) => terms.seek_exact_with_ord(ord),
      Self::B(terms) => terms.seek_exact_with_ord(ord),
    }
  }

  fn seek_exact_with_state<BS: ByteSource>(
    &mut self,
    term: &BytesRef<BS>,
    state: &TermStateEnum,
  ) -> Result<()> {
    match self {
      Self::A(terms) => terms.seek_exact_with_state(term, state),
      Self::B(terms) => terms.seek_exact_with_state(term, state),
    }
  }

  fn term(&self) -> Result<Self::Value<'_>> {
    match self {
      Self::A(terms) => terms.term().map(BytesRefValue::into_value),
      Self::B(terms) => terms.term().map(BytesRefValue::into_value),
    }
  }

  fn ord(&self) -> Result<i64> {
    match self {
      Self::A(terms) => terms.ord(),
      Self::B(terms) => terms.ord(),
    }
  }

  fn doc_freq(&mut self) -> Result<i32> {
    match self {
      Self::A(terms) => terms.doc_freq(),
      Self::B(terms) => terms.doc_freq(),
    }
  }

  fn total_term_freq(&mut self) -> Result<i64> {
    match self {
      Self::A(terms) => terms.total_term_freq(),
      Self::B(terms) => terms.total_term_freq(),
    }
  }

  type PostingsEnum = <<SortedDocValuesWrap<SortedSet<LR>> as SortedDocValues>::TermsEnum<'a> as TermsEnum>::PostingsEnum;

  fn postings(&mut self, reuse: Option<Self::PostingsEnum>) -> Result<Self::PostingsEnum> {
    match self {
      Self::A(terms) => terms.postings(reuse),
      Self::B(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn postings_with_flags(
    &mut self,
    reuse: Option<Self::PostingsEnum>,
    flags: i32,
  ) -> Result<Self::PostingsEnum> {
    match self {
      Self::A(terms) => terms.postings_with_flags(reuse, flags),
      Self::B(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  type ImpactsEnum = <<SortedDocValuesWrap<SortedSet<LR>> as SortedDocValues>::TermsEnum<'a> as TermsEnum>::ImpactsEnum;

  fn impacts(&mut self, flags: i32) -> Result<Self::ImpactsEnum> {
    match self {
      Self::A(terms) => terms.impacts(flags),
      Self::B(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn term_state(&mut self) -> Result<TermStateEnum> {
    match self {
      Self::A(terms) => terms.term_state(),
      Self::B(terms) => terms.term_state(),
    }
  }
}

impl<LR> SortedDocValues for TermOrdValDocValues<LR>
where
  LR: LeafReader,
{
  type OrdValue<'a>
    = BytesRefValueEnum<'a>
  where
    Self: 'a;

  fn ord_value(&mut self) -> Result<i32> {
    match self {
      Self::A(values) => values.ord_value(),
      Self::B(values) => values.ord_value(),
    }
  }

  fn lookup_ord(&mut self, ord: i32) -> Result<Self::OrdValue<'_>> {
    match self {
      Self::A(values) => values.lookup_ord(ord).map(BytesRefValue::into_value),
      Self::B(values) => values.lookup_ord(ord).map(BytesRefValue::into_value),
    }
  }

  fn get_value_count(&self) -> Result<i32> {
    match self {
      Self::A(values) => values.get_value_count(),
      Self::B(values) => values.get_value_count(),
    }
  }

  fn lookup_term<BS: ByteSource>(&mut self, key: &BytesRef<BS>) -> Result<i32> {
    match self {
      Self::A(values) => values.lookup_term(key),
      Self::B(values) => values.lookup_term(key),
    }
  }

  type TermsEnum<'a>
    = TermOrdValTermsEnum<'a, LR>
  where
    LR: 'a,
    LR::SortedDocValues: 'a,
    LR::SortedSetDocValues: 'a;

  fn terms_enum(&mut self) -> Result<Self::TermsEnum<'_>> {
    match self {
      Self::A(values) => values.terms_enum().map(TermOrdValTermsEnum::A),
      Self::B(values) => values.terms_enum().map(TermOrdValTermsEnum::B),
    }
  }
}
