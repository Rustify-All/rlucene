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
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::search::comparators::min_doc_iterator::MinDocIterator;
use crate::core::search::doc_id_set_iterator::{
    AllDISI, DocIdSetIterator, Either3DocIdSetIterator, EmptyDISI,
};
use crate::core::search::field_comparator::FieldComparator;
use crate::core::search::leaf_field_comparator::LeafFieldComparator;
use crate::core::search::pruning::Pruning;
use crate::core::search::scorable::{Scorable, ScorerEnum};
use crate::core::search::scorer::Scorer;
use crate::core::util::ToInt;
use crate::core::util::error::lucene_error::Result;

/// Comparator that sorts by asc _doc
pub struct DocComparator {
    doc_ids: Vec<i32>,
    // if skipping functionality should be enabled
    enable_skipping: bool,
    bottom: i32,
    top_value: i32,
    top_value_set: bool,
    bottom_value_set: bool,
    hits_threshold_reached: bool,
}

impl DocComparator {
    /// Creates a new comparator based on document ids for `num_hits`.
    pub fn new(num_hits: usize, reverse: bool, pruning: Pruning) -> Self {
        // skipping functionality is enabled if we are sorting by _doc in asc order as a primary sort
        let enable_skipping = !reverse && pruning != Pruning::None;
        Self {
            doc_ids: vec![0; num_hits],
            enable_skipping,
            bottom: 0,
            top_value: 0,
            top_value_set: false,
            bottom_value_set: false,
            hits_threshold_reached: false,
        }
    }
}
impl FieldComparator for DocComparator {
    type V = i32;

    fn compare(&self, slot1: i32, slot2: i32) -> i32 {
        self.doc_ids[slot1 as usize] - self.doc_ids[slot2 as usize]
    }

    fn set_top_value(&mut self, value: Self::V) {
        self.top_value = value;
        self.top_value_set = true;
    }

    fn value(&self, slot: i32) -> &Self::V {
        &self.doc_ids[slot as usize]
    }

    type LeafFieldComparator<LR>
        = DocLeafComparator
    where
        LR: LeafReader;

    fn get_leaf_comparator<LR>(
        self,
        context: &LeafReaderContext<LR>,
    ) -> Result<Self::LeafFieldComparator<LR>>
    where
        LR: LeafReader,
    {
        DocLeafComparator::new(context, self)
    }
}
/// DocLeafComparator with skipping functionality.
///
/// When sorting by `_doc` ascending:
///
/// - After collecting top **N** matches and enough hits, the comparator can skip all the following documents.
/// - When sorting by `_doc` ascending and a "top" document is set (after which search should start),
///   the comparator provides an iterator that can quickly skip to the desired "top" document.
pub struct DocLeafComparator {
    doc_base: i32,
    min_doc: i32,
    max_doc: i32,
    competitive_iterator: Option<DocComparatorCompetitiveIterator>,
    comparator: DocComparator,
}

impl DocLeafComparator {
    pub fn new<LR>(context: &LeafReaderContext<LR>, comparator: DocComparator) -> Result<Self>
    where
        LR: LeafReader,
    {
        let doc_base = context.doc_base;

        let (min_doc, max_doc, competitive_iterator) = if comparator.enable_skipping {
            // Skip docs before topValue, but include docs starting with topValue.
            // Including topValue is necessary when doing sort on [_doc, other fields]
            // in a distributed search where there are docs from different indices
            // with the same docID.
            let min_doc = comparator.top_value;
            let max_doc = context.reader().max_doc()?;
            let it = Some(DocComparatorCompetitiveIterator::A(AllDISI::new(max_doc)));
            (min_doc, max_doc, it)
        } else {
            (-1, -1, None)
        };

        Ok(Self {
            doc_base,
            min_doc,
            max_doc,
            competitive_iterator,
            comparator,
        })
    }
    fn update_iterator(&mut self) {
        if !self.comparator.enable_skipping || !self.comparator.hits_threshold_reached {
            return;
        }

        if self.comparator.bottom_value_set {
            self.competitive_iterator =
                Some(DocComparatorCompetitiveIterator::B(EmptyDISI::default()));
        } else if self.comparator.top_value_set {
            // since we've collected top N matches, we can early terminate
            // Currently early termination on _doc is also implemented in TopFieldCollector, but this
            // will be removed
            // once all bulk scores uses collectors' iterators
            if self.doc_base + self.max_doc <= self.min_doc {
                self.competitive_iterator =
                    Some(DocComparatorCompetitiveIterator::B(EmptyDISI::default())); // skip this segment
            } else {
                let current_doc = self
                    .competitive_iterator
                    .as_ref()
                    .expect("competitive_iterator must be initialized before update_iterator")
                    .doc_id();
                let segment_min_doc = current_doc.max(self.min_doc - self.doc_base);

                self.competitive_iterator = Some(DocComparatorCompetitiveIterator::C(
                    MinDocIterator::new(segment_min_doc, self.max_doc),
                ));
            }
        }
    }
}
impl LeafFieldComparator for DocLeafComparator {
    fn set_bottom(&mut self, slot: usize) -> Result<()> {
        self.comparator.bottom = self.comparator.doc_ids[slot];
        self.comparator.bottom_value_set = true;
        self.update_iterator();
        Ok(())
    }

    fn compare_bottom(&mut self, doc: i32) -> Result<i32> {
        // No overflow risk because docIDs are non-negative
        Ok(self.comparator.bottom - (self.doc_base + doc))
    }

    fn compare_top(&mut self, doc: i32) -> Result<i32> {
        let doc_value = self.doc_base + doc;
        Ok(self.comparator.top_value.cmp(&doc_value).to_int())
    }

    fn copy(&mut self, slot: usize, doc: i32) -> Result<()> {
        self.comparator.doc_ids[slot] = self.doc_base + doc;
        Ok(())
    }

    fn set_scorer<S1, S2>(&mut self, _scorer: &ScorerEnum<S1, S2>) -> Result<()>
    where
        S1: Scorer,
        S2: Scorable,
    {
        self.update_iterator();
        Ok(())
    }

    type DocIdSetIterator = DocComparatorIterator;

    fn competitive_iterator(&mut self) -> Option<Self::DocIdSetIterator> {
        debug_assert!(self.competitive_iterator.is_some());
        Some(DocComparatorIterator::new(
            self.competitive_iterator.take().unwrap(),
        ))
    }

    fn set_hits_threshold_reached(&mut self) -> Result<()> {
        self.comparator.hits_threshold_reached = true;
        self.update_iterator();
        Ok(())
    }
}

pub type DocComparatorCompetitiveIterator =
    Either3DocIdSetIterator<AllDISI, EmptyDISI, MinDocIterator>;
pub struct DocComparatorIterator {
    competitive_iterator: DocComparatorCompetitiveIterator,
    doc_id: i32,
}
impl DocComparatorIterator {
    pub fn new(competitive_iterator: DocComparatorCompetitiveIterator) -> Self {
        Self {
            competitive_iterator,
            doc_id: 0,
        }
    }
}
impl DocIdSetIterator for DocComparatorIterator {
    fn doc_id(&self) -> i32 {
        self.competitive_iterator.doc_id()
    }

    fn next_doc(&mut self) -> Result<i32> {
        self.advance(self.doc_id() + 1)
    }

    fn advance(&mut self, target: i32) -> Result<i32> {
        self.doc_id = self.competitive_iterator.advance(target)?;
        Ok(self.doc_id)
    }

    fn cost(&self) -> Result<i64> {
        self.competitive_iterator.cost()
    }
}
