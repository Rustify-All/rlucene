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
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::search::two_phase_iterator::TwoPhaseIterator;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::bit_set::BitSet;
use crate::core::util::bit_set_iterator::BitSetIterator;
use crate::core::util::collection_util::CollectionUtil;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::{Comparator, ToInt, TryIntoInt};

pub struct ConjunctionDISI<D>
where
    D: DocIdSetIterator,
{
    lead1: D,
    lead2: D,
    others: Vec<D>,
}
impl<D> ConjunctionDISI<D>
where
    D: DocIdSetIterator,
{
    fn new(iterators: Vec<D>) -> Result<Self> {
        debug_assert!(iterators.len() >= 2);
        let mut cost = Vec::with_capacity(iterators.len());
        let mut temp_iterators = Vec::with_capacity(iterators.len());
        for (idx, v) in iterators.into_iter().enumerate() {
            cost.push(idx);
            temp_iterators.push(Some(v));
        }
        let cmp = DisiCmp::new(temp_iterators.as_ref());
        CollectionUtil::tim_sort_with_comparator(&mut cost, cmp)?;
        let mut iters = Vec::with_capacity(temp_iterators.len());
        for idx in cost {
            iters.push(temp_iterators[idx].take().unwrap());
        }
        let lead1 = iters.remove(0);
        let lead2 = iters.remove(0);
        Ok(Self {
            lead1,
            lead2,
            others: iters,
        })
    }
    fn do_next(&mut self, mut doc: i32) -> Result<i32> {
        'advance_head: loop {
            debug_assert_eq!(doc, self.lead1.doc_id());
            // find agreement between the two iterators with the lower costs
            // we special case them because they do not need the
            // 'other.docID() < doc' check that the 'others' iterators need
            let next2 = self.lead2.advance(doc)?;
            if next2 != doc {
                doc = self.lead1.advance(next2)?;
                if doc != next2 {
                    continue;
                }
            }
            // then find agreement with other iterators
            for other in &mut self.others {
                let other_doc = other.doc_id();
                // other.doc may already be equal to doc if we "continued advanceHead"
                // on the previous iteration and the advance on the lead scorer exactly matched.
                if other_doc < doc {
                    let next = other.advance(doc)?;

                    if next > doc {
                        // iterator beyond the current doc - advance lead and continue to the new highest doc.
                        doc = self.lead1.advance(next)?;
                        continue 'advance_head;
                    }
                }
            }
            return Ok(doc);
        }
    }
    // Returns {@code true} if all sub-iterators are on the same doc ID, {@code false} otherwise
    fn assert_iters_on_same_doc(&self) -> bool {
        let cur_doc = self.lead1.doc_id();
        let mut iterators_on_the_same_doc = self.lead2.doc_id() == cur_doc;
        let mut i = 0;
        while i < self.others.len() && iterators_on_the_same_doc {
            iterators_on_the_same_doc =
                iterators_on_the_same_doc && (self.others[i].doc_id() == cur_doc);
            i += 1;
        }
        iterators_on_the_same_doc
    }
}
impl<D> DocIdSetIterator for ConjunctionDISI<D>
where
    D: DocIdSetIterator,
{
    fn doc_id(&self) -> i32 {
        self.lead1.doc_id()
    }

    fn next_doc(&mut self) -> Result<i32> {
        debug_assert!(
            self.assert_iters_on_same_doc(),
            "Sub-iterators of ConjunctionDISI are not on the same document!"
        );
        let doc = self.lead1.next_doc()?;
        self.do_next(doc)
    }

    fn advance(&mut self, target: i32) -> Result<i32> {
        debug_assert!(
            self.assert_iters_on_same_doc(),
            "Sub-iterators of ConjunctionDISI are not on the same document!"
        );
        let doc = self.lead1.advance(target)?;
        self.do_next(doc)
    }

    fn cost(&self) -> Result<i64> {
        self.lead1.cost()
    }
}

struct DisiCmp<'a, D>
where
    D: DocIdSetIterator,
{
    disi: &'a [Option<D>],
}
impl<'a, D> DisiCmp<'a, D>
where
    D: DocIdSetIterator,
{
    fn new(disi: &'a [Option<D>]) -> Self {
        DisiCmp { disi }
    }
}
impl<D> Comparator<usize> for DisiCmp<'_, D>
where
    D: DocIdSetIterator,
{
    const TYPE: &'static str = "DisiCmp";

    fn compare(&self, a: &usize, b: &usize) -> Result<i32> {
        Ok(self.disi[*a]
            .as_ref()
            .unwrap()
            .cost()?
            .cmp(&self.disi[*b].as_ref().unwrap().cost()?)
            .to_int())
    }
}
/// Conjunction between a [`DocIdSetIterator`] and one or more BitSetIterators.
pub struct BitSetConjunctionDISI<DISI, T>
where
    DISI: DocIdSetIterator,
    T: BitSet,
{
    lead: DISI,
    bit_set_iterators: Vec<BitSetIterator<T>>,
    min_length: usize,
}
impl<DISI, T> BitSetConjunctionDISI<DISI, T>
where
    DISI: DocIdSetIterator,
    T: BitSet,
{
    pub fn new(lead: DISI, bit_set_iterators: Vec<BitSetIterator<T>>) -> Result<Self> {
        assert!(!bit_set_iterators.is_empty());
        let mut temp_bit_set_iterators = Vec::with_capacity(bit_set_iterators.len());
        let mut cost = Vec::with_capacity(bit_set_iterators.len());
        for (idx, v) in bit_set_iterators.into_iter().enumerate() {
            cost.push(idx);
            temp_bit_set_iterators.push(Some(v));
        }
        let cmp = BitSetIteratorCmp::new(temp_bit_set_iterators.as_ref());
        ArrayUtil::tim_sort_with_comparator(&mut cost, cmp)?;

        let bit_set_iterators = cost
            .into_iter()
            .map(|idx| temp_bit_set_iterators[idx].take().unwrap())
            .collect::<Vec<_>>();
        let mut min_length = i32::MAX;
        for iter in &bit_set_iterators {
            let bit_set = iter.get_bit_set();
            min_length = min_length.min(bit_set.length() as i32);
        }

        Ok(Self {
            lead,
            bit_set_iterators,
            min_length: min_length.try_convert()?,
        })
    }
    fn do_next(&mut self, mut doc: i32) -> Result<i32> {
        'advance_lead: loop {
            if doc >= self.min_length as i32 {
                if doc != NO_MORE_DOCS {
                    self.lead.advance(NO_MORE_DOCS)?;
                }
                return Ok(NO_MORE_DOCS);
            }

            for bs_iter in &self.bit_set_iterators {
                let bs = bs_iter.get_bit_set();
                if !bs.get(doc as usize)? {
                    doc = self.lead.next_doc()?;
                    continue 'advance_lead;
                }
            }

            for iter in &mut self.bit_set_iterators {
                iter.set_doc_id(doc);
            }

            return Ok(doc);
        }
    }
    fn assert_iters_on_same_doc(&self) -> bool {
        let cur_doc = self.lead.doc_id();
        for iter in &self.bit_set_iterators {
            if iter.doc_id() != cur_doc {
                return false;
            }
        }
        true
    }
}
impl<DISI, T> DocIdSetIterator for BitSetConjunctionDISI<DISI, T>
where
    DISI: DocIdSetIterator,
    T: BitSet,
{
    fn doc_id(&self) -> i32 {
        self.lead.doc_id()
    }

    fn next_doc(&mut self) -> Result<i32> {
        debug_assert!(
            self.assert_iters_on_same_doc(),
            "Sub-iterators of BitSetConjunctionDISI are not on the same document!"
        );
        let doc = self.lead.next_doc()?;
        self.do_next(doc)
    }

    fn advance(&mut self, target: i32) -> Result<i32> {
        debug_assert!(
            self.assert_iters_on_same_doc(),
            "Sub-iterators of BitSetConjunctionDISI are not on the same document!"
        );
        let doc = self.lead.advance(target)?;
        self.do_next(doc)
    }

    fn cost(&self) -> Result<i64> {
        self.lead.cost()
    }
}
struct BitSetIteratorCmp<'a, B>
where
    B: BitSet,
{
    disi: &'a [Option<BitSetIterator<B>>],
}
impl<'a, B> BitSetIteratorCmp<'a, B>
where
    B: BitSet,
{
    fn new(disi: &'a [Option<BitSetIterator<B>>]) -> Self {
        BitSetIteratorCmp { disi }
    }
}
impl<B> Comparator<usize> for BitSetIteratorCmp<'_, B>
where
    B: BitSet,
{
    const TYPE: &'static str = "BitSetIteratorCmp";

    fn compare(&self, a: &usize, b: &usize) -> Result<i32> {
        Ok(self.disi[*a]
            .as_ref()
            .unwrap()
            .cost()?
            .cmp(&self.disi[*b].as_ref().unwrap().cost()?)
            .to_int())
    }
}
/// [`TwoPhaseIterator`] implementing a conjunction.
pub struct ConjunctionTwoPhaseIterator<T, D>
where
    T: TwoPhaseIterator,
    D: DocIdSetIterator,
{
    two_phase_iterators: Vec<T>,
    approximation: D,
    match_cost: f32,
}
impl<T, D> ConjunctionTwoPhaseIterator<T, D>
where
    T: TwoPhaseIterator,
    D: DocIdSetIterator,
{
    pub fn new(approximation: D, two_phase_iterators: Vec<T>, match_cost: f32) -> Self {
        debug_assert!(!two_phase_iterators.is_empty());
        let mut temp_two_phase_iterators = Vec::with_capacity(two_phase_iterators.len());
        let mut cost = Vec::with_capacity(two_phase_iterators.len());
        for (idx, v) in two_phase_iterators.into_iter().enumerate() {
            cost.push(idx);
            temp_two_phase_iterators.push(Some(v));
        }
        let cmp = TwoPhaseIteratorCmp::new(temp_two_phase_iterators.as_ref());
        ArrayUtil::tim_sort_with_comparator(&mut cost, cmp).unwrap();
        let two_phase_iterators = cost
            .into_iter()
            .map(|idx| temp_two_phase_iterators[idx].take().unwrap())
            .collect::<Vec<_>>();

        ConjunctionTwoPhaseIterator {
            two_phase_iterators,
            approximation,
            match_cost,
        }
    }
}
impl<T, D> TwoPhaseIterator for ConjunctionTwoPhaseIterator<T, D>
where
    T: TwoPhaseIterator,
    D: DocIdSetIterator,
{
    type DocIdSetIterator = D;
    type DocIdSetIteratorRef<'a>
        = &'a D
    where
        Self: 'a;
    type DocIdSetIteratorMut<'a>
        = &'a mut D
    where
        Self: 'a;

    fn approximation_mut(&mut self) -> Result<Self::DocIdSetIteratorMut<'_>> {
        Ok(&mut self.approximation)
    }

    fn approximation(&self) -> Result<Self::DocIdSetIteratorRef<'_>> {
        Ok(&self.approximation)
    }

    fn matches(&mut self) -> Result<bool> {
        for x in &mut self.two_phase_iterators {
            if !x.matches()? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn match_cost(&self) -> f32 {
        self.match_cost
    }
}
struct TwoPhaseIteratorCmp<'a, T>
where
    T: TwoPhaseIterator,
{
    tpi: &'a [Option<T>],
}
impl<'a, T> TwoPhaseIteratorCmp<'a, T>
where
    T: TwoPhaseIterator,
{
    fn new(disi: &'a [Option<T>]) -> Self {
        TwoPhaseIteratorCmp { tpi: disi }
    }
}
impl<T> Comparator<usize> for TwoPhaseIteratorCmp<'_, T>
where
    T: TwoPhaseIterator,
{
    const TYPE: &'static str = "TwoPhaseIteratorCmp";

    fn compare(&self, a: &usize, b: &usize) -> Result<i32> {
        Ok(self.tpi[*a]
            .as_ref()
            .unwrap()
            .match_cost()
            .partial_cmp(&self.tpi[*b].as_ref().unwrap().match_cost())
            .unwrap()
            .to_int())
    }
}
