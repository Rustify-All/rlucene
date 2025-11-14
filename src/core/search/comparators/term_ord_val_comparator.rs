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
use crate::core::index::BytesRef;
use crate::core::index::doc_values::{DocValues, Sorted, SortedSet};
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::leaf_reader::{LRPosting, LRTermsEnum, LeafReader};
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::postings_enum::NONE;
use crate::core::index::sorted_doc_values::{Either2SortedDocValues, SortedDocValues};
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::TermsEnum;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::search::field_comparator::FieldComparator;
use crate::core::search::index_searcher::get_max_clause_count;
use crate::core::search::leaf_field_comparator::LeafFieldComparator;
use crate::core::search::pruning::Pruning;
use crate::core::search::scorable::Scorable;
use crate::core::search::sorted_set_selector::SortedDocValuesWrap;
use crate::core::util::ToInt;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::priority_queue::{Compare, PriorityQueue};
use std::collections::VecDeque;

/// Sorts by field's natural Term sort order, using ordinals.
///
/// This is functionally equivalent to
/// [`TermValComparator`](crate::core::search::field_comparator::TermValComparator),
/// but it first resolves the string to their relative ordinal positions
/// (using the index returned by
/// [`LeafReader::getSortedDocValues`](LeafReader::get_sorted_doc_values)),
/// and does most comparisons using the ordinals.
///
/// For medium to large results, this comparator will be much faster than
/// [`TermValComparator`](crate::core::search::field_comparator::TermValComparator).
/// For very small result sets it may be slower.
pub struct TermOrdValComparator {
    /// Ords for each slot.
    pub(crate) ords: Vec<i32>,
    /// Values for each slot.
    pub(crate) values: Vec<Option<BytesRef<Vec<u8>>>>,
    /// Which reader last copied a value into the slot. When
    ///   we compare two slots, we just compare-by-ord if the
    ///  readerGen is the same; else we must compare the
    ///  values (slower).
    pub(crate) reader_gen: Vec<i32>,
    /// Gen of current reader we are on.
    pub(crate) current_reader_gen: i32,
    pub(crate) field: String,
    reverse: bool,
    sort_missing_last: bool,
    /// Bottom value (same as `values[bottomSlot]` once bottomSlot is set).  Cached for faster compares.
    pub(crate) bottom_value: Option<usize>,
    /* Bottom slot, or -1 if queue isn't full yet */
    pub(crate) bottom_slot: i32,
    /// Set by setTopValue.
    pub(crate) top_value: Option<BytesRef<Vec<u8>>>,
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
    pub fn new(
        field: String,
        num_hits: usize,
        sort_missing_last: bool,
        reverse: bool,
        pruning: Pruning,
    ) -> Self {
        let can_skip_documents = pruning != Pruning::None;
        Self {
            ords: vec![0; num_hits],
            values: vec![None; num_hits],
            reader_gen: vec![0; num_hits],
            current_reader_gen: -1,
            field,
            reverse,
            sort_missing_last,
            bottom_value: None,
            bottom_slot: -1,
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

    fn compare(&self, slot1: i32, slot2: i32) -> i32 {
        let slot1 = slot1 as usize;
        let slot2 = slot2 as usize;
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

    fn set_top_value(&mut self, value: Self::V) {
        // None is fine: it means the last doc of the prior
        // search was missing this value
        self.top_value = Some(value);
    }

    fn value(&self, slot: i32) -> Option<Self::V> {
        // TODO: IMPORTANT: avoid the clone here
        self.values[slot as usize].clone()
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
        let c = |context: &LeafReaderContext<LR>| -> Result<TermOrdValDocValues<LR>> {
            Ok(TermOrdValDocValues::<LR>::B(get_sorted_doc_values(
                context,
                &self.field,
            )?))
        };
        TermOrdValLeafComparator::new(context, c, self)
    }

    fn compare_values(&self, val1: Option<&Self::V>, val2: Option<&Self::V>) -> i32 {
        match (val1, val2) {
            (None, None) => 0,
            (None, Some(_)) => self.missing_sort_cmp,
            (Some(_), None) => -self.missing_sort_cmp,
            (Some(v1), Some(v2)) => v1.cmp(v2).to_int(),
        }
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
        comparator: &TermOrdValComparator,
    ) -> Result<Self>
    where
        F: Fn(&LeafReaderContext<LR>) -> Result<TermOrdValDocValues<LR>>,
    {
        let mut terms_index = doc_value_producer(context)?;
        let missing_ord = if comparator.sort_missing_last {
            i32::MAX
        } else {
            -1
        };
        let (top_ord, top_same_reader) = if let Some(ref top_value) = comparator.top_value {
            // Recompute topOrd/SameReader
            let ord = terms_index.lookup_term(top_value)?;
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

        // TODO
        // if comparator.bottom_slot != -1 {
        //     leaf.set_bottom(comparator.bottom_slot as usize)?;
        // }

        let enable_skipping = if !comparator.can_skip_documents {
            leaf.dense = false;
            false
        } else {
            let field_info = context
                .reader()
                .get_field_infos()?
                .field_info_by_name(&comparator.field);
            if field_info.is_none() {
                if leaf.terms_index.get_value_count()? != 0 {
                    return Err(LuceneError::illegal_state(format!(
                        "Field [{}] cannot be found in field infos",
                        comparator.field
                    )));
                }
                leaf.dense = false;
                true
            } else if *field_info.unwrap().get_index_options() == IndexOptions::None {
                // No terms index
                leaf.dense = false;
                false
            } else {
                let terms = context.reader().terms(&comparator.field)?;
                match terms {
                    None => {
                        leaf.dense = false;
                    },
                    Some(ref t) => {
                        leaf.dense = t.get_sum_doc_freq()? == context.reader().max_doc()? as i64;
                    },
                }

                if leaf.dense || comparator.top_value.is_some() {
                    true
                } else if comparator.reverse == comparator.sort_missing_last {
                    // Missing values are always competitive, we can never skip
                    false
                } else {
                    true
                }
            }
        };

        if enable_skipping {
            let docs_with_field = match leaf.dense {
                true => None,
                false => Some(doc_value_producer(context)?),
            };
            let terms = context.reader().terms(&comparator.field)?;
            let terms_enum = match terms {
                None => {
                    return Err(LuceneError::illegal_state("terms is None"));
                },
                Some(terms_enum) => terms_enum.iterator()?,
            };
            leaf.competitive_iterator = Some(TermOrdValCompetitiveIterator::new(
                context,
                leaf.dense,
                docs_with_field,
                terms_enum,
            )?);
        }

        leaf.update_competitive_iterator(comparator)?;

        Ok(leaf)
    }
    fn update_competitive_iterator(&mut self, comparator: &TermOrdValComparator) -> Result<()> {
        if self.competitive_iterator.is_none()
            || !comparator.hits_threshold_reached
            || comparator.bottom_slot == -1
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

        self.competitive_iterator.as_mut().unwrap().update(
            &mut self.terms_index,
            min_ord,
            max_ord,
        )?;

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
    fn set_bottom(&mut self, bottom: usize, comparator: &mut Self::FieldComparator) -> Result<()> {
        comparator.bottom_slot = bottom as i32;
        comparator.bottom_value = Some(bottom);

        if comparator.current_reader_gen == comparator.reader_gen[bottom] {
            self.bottom_ord = comparator.ords[bottom];
            self.bottom_same_reader = true;
        } else {
            let has_value = comparator.values[bottom].is_some();
            match has_value {
                false => {
                    // missingOrd is null for all segments
                    debug_assert!(comparator.ords[bottom] == self.missing_ord);
                    self.bottom_ord = self.missing_ord;
                    self.bottom_same_reader = true;
                    comparator.reader_gen[bottom] = comparator.current_reader_gen;
                },
                true => {
                    let target = match comparator.values[bottom].as_ref() {
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
                        comparator.reader_gen[bottom] = comparator.current_reader_gen;
                        comparator.ords[bottom] = self.bottom_ord;
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
        S: Scorable,
    {
        debug_assert!(comparator.bottom_slot != -1);

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
        S: Scorable,
    {
        let mut ord = self.get_ord_for_doc(doc)?;
        if ord == -1 {
            ord = self.missing_ord;
        }

        if self.top_same_reader {
            // ord is precisely comparable, even in the equal case
            // System.out.println("compareTop doc=" + doc + " ord=" + ord + " ret=" + (topOrd-ord));
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
        S: Scorable,
    {
        let mut ord = self.get_ord_for_doc(doc)?;
        if ord == -1 {
            ord = self.missing_ord;
            comparator.values[slot] = None;
        } else {
            debug_assert!(ord >= 0);

            let v = self.terms_index.lookup_ord(ord)?.into_owned();
            comparator.values[slot] = Some(v);
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
        S: Scorable,
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

const MAX_TERMS: i32 = 1024;
pub struct TermOrdValCompetitiveIterator<LR>
where
    LR: LeafReader,
{
    max_doc: i32,
    dense: bool,
    doc: i32,
    postings: VecDeque<i32>,
    postings_init: bool,
    terms_enum: Option<LRTermsEnum<LR>>,
    docs_with_field: Option<TermOrdValDocValues<LR>>,
    // if docs_with_field is active, dense must be false
    using_skip: bool,
    disjunction: Option<PriorityQueue<PostingsEnumAndOrd<LR>, PostingsEnumAndOrdCmp>>,
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
            terms_enum: Some(terms_enum),
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
        let size = std::cmp::max(0, max_ord - min_ord + 1);

        if size > max_terms {
            self.using_skip = true;
        } else if !self.postings_init {
            self.init(doc_values, min_ord, max_ord)?;
        } else if size < self.postings.len() as i32 {
            // One or more ords got removed
            debug_assert!(self.postings.is_empty() || *self.postings.front().unwrap() <= min_ord);
            while !self.postings.is_empty() && *self.postings.front().unwrap() < min_ord {
                self.postings.pop_front();
            }

            debug_assert!(self.postings.is_empty() || *self.postings.back().unwrap() >= max_ord);
            while !self.postings.is_empty() && *self.postings.back().unwrap() > max_ord {
                self.postings.pop_back();
            }
            let disjunction = self.disjunction.as_mut().unwrap();
            let iterms = disjunction.take_heap_array();
            debug_assert!(
                iterms.len() == self.postings.len(),
                "priority queue size must match postings size"
            );
            let (min_ord, max_ord) = if !self.postings.is_empty() {
                (
                    *self.postings.front().unwrap(),
                    *self.postings.back().unwrap(),
                )
            } else {
                (0, 0)
            };
            for v in iterms {
                if v.ord < min_ord || v.ord > max_ord {
                    // this ord was removed
                    continue;
                }
                disjunction.add(v)?;
            }
        } else {
            self.init(doc_values, min_ord, max_ord)?;
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
        let size = std::cmp::max(0, max_ord - min_ord + 1);
        self.postings = VecDeque::with_capacity(size as usize);

        debug_assert!(self.disjunction.is_none());
        let mut disjunction = PriorityQueue::new(size, PostingsEnumAndOrdCmp)?;
        if size > 0 {
            let min_term = doc_values.lookup_ord(min_ord)?.into_owned();
            let terms = self
                .terms_enum
                .as_mut()
                .ok_or_else(|| LuceneError::IllegalState("terms_enum not initialized".into()))?;

            if !terms.seek_exact(&min_term)? {
                return Err(LuceneError::illegal_state(format!(
                    "Term {} exists in doc values but not in the terms index",
                    min_term
                )));
            }

            disjunction.add(PostingsEnumAndOrd::<LR>::new(
                terms.postings_with_flags(None, NONE as i32)?,
                min_ord,
            ))?;
            self.postings.push_back(min_ord);

            for ord in (min_ord + 1)..=max_ord {
                let next = terms.next()?;
                let next = match next {
                    Some(term) => term,
                    None => {
                        return Err(LuceneError::illegal_state(format!(
                            "Terms have more than {ord} unique terms while doc values have exactly {ord} terms"
                        )));
                    },
                };

                let expected_ord = doc_values.lookup_term(next.as_ref())?;
                debug_assert!(
                    expected_ord == ord,
                    "docValuesTerms not aligned with terms index"
                );
                disjunction.add(PostingsEnumAndOrd::new(
                    terms.postings_with_flags(None, NONE as i32)?,
                    ord,
                ))?;
                self.postings.push_back(ord);
            }
        }
        self.disjunction = Some(disjunction);
        Ok(())
    }
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

        if self.disjunction.is_none() {
            if self.using_skip {
                // The field is sparse and we're only interested in documents that have a value.
                debug_assert!(!self.dense);
                self.doc = self.docs_with_field.as_mut().unwrap().advance(target)?;
                return Ok(self.doc);
            } else {
                // We haven't started skipping yet
                self.doc = target;
                return Ok(self.doc);
            }
        }

        let disjunction = self.disjunction.as_mut().unwrap();
        let top = disjunction.top_mut();

        if top.is_none() {
            // priority queue is empty, none of the remaining documents are competitive
            self.doc = NO_MORE_DOCS;
            return Ok(self.doc);
        }

        let mut top = top.unwrap();

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
struct PostingsEnumAndOrd<LR>
where
    LR: LeafReader,
{
    postings: LRPosting<LR>,
    ord: i32,
}
impl<LR> PostingsEnumAndOrd<LR>
where
    LR: LeafReader,
{
    pub fn new(postings: LRPosting<LR>, ord: i32) -> Self {
        Self { postings, ord }
    }
}
struct PostingsEnumAndOrdCmp;
impl<LR> Compare<PostingsEnumAndOrd<LR>> for PostingsEnumAndOrdCmp
where
    LR: LeafReader,
{
    fn less_than(&self, a: &PostingsEnumAndOrd<LR>, b: &PostingsEnumAndOrd<LR>) -> Result<bool> {
        Ok(a.postings.doc_id() < b.postings.doc_id())
    }
}
pub type TermOrdValDocValues<LR> =
    Either2SortedDocValues<SortedDocValuesWrap<SortedSet<LR>>, Sorted<LR>>;
