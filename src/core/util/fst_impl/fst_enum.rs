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
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fst_impl::fst::{
    ARCS_FOR_BINARY_SEARCH, ARCS_FOR_CONTINUOUS, ARCS_FOR_DIRECT_ADDRESSING, Arc, BitTable,
    END_LABEL, FST,
};
use crate::core::util::fst_impl::fst_reader::FstReader;
use crate::core::util::fst_impl::outputs::Outputs;
use crate::core::util::fst_impl::util::Util;

/// Can next() and advance() through the terms in an FST
pub struct FSTEnum<O, F>
where
    O: Outputs,
    F: FstReader,
{
    pub fst: FST<O, F>,
    pub(crate) arcs: Vec<Arc<O::V>>,
    pub(crate) output: Vec<O::V>,

    pub(crate) no_output: O::V,
    pub(crate) fst_reader: F::FstBytesReader,
    pub(crate) upto: usize,
    pub(crate) target_length: i32,
}
impl<O, F> FSTEnum<O, F>
where
    O: Outputs,
    F: FstReader,
{
    pub(crate) fn new(fst: FST<O, F>) -> Result<Self> {
        let fst_reader = fst.get_bytes_reader()?;
        let no_output = fst.outputs.get_no_output();
        let mut arcs = vec![Arc::default(); 10];
        let mut arc = Arc::default();
        fst.get_first_arc(&mut arc);
        arcs[0] = arc;

        let mut output = vec![O::V::default(); 10];
        output[0] = no_output.clone();

        Ok(Self {
            fst,
            arcs,
            output,
            no_output,
            fst_reader,
            upto: 0,
            target_length: 0,
        })
    }
    /// Rewinds enum state to match the shared prefix between current term and
    /// target term.
    fn rewind_prefix<FB>(&mut self, sub: &mut FB, target: &FB::V) -> Result<()>
    where
        FB: FSTEnumBase<O, F>,
    {
        // let fst = self.fst.borrow_mut();
        if self.upto == 0 {
            self.upto = 1;
            let (left, right) = self.arcs.split_at_mut(1);
            let arc0 = &left[0];
            let arc1 = &mut right[0];
            self.fst
                .read_first_target_arc(arc0, arc1, &mut self.fst_reader)?;
            return Ok(());
        }

        let current_limit = self.upto;
        self.upto = 1;

        {
            // Borrow fst mutably once for the entire loop.
            while self.upto < current_limit && self.upto <= self.target_length as usize + 1 {
                let cmp = sub.get_current_label(self)? - sub.get_target_label(self, target)?;
                if cmp < 0 {
                    break;
                } else if cmp > 0 {
                    let (left, right) = self.arcs.split_at_mut(self.upto);
                    let arc_prev = &left[self.upto - 1];
                    let arc = &mut right[0];
                    self.fst
                        .read_first_target_arc(arc_prev, arc, &mut self.fst_reader)?;
                    break;
                }
                self.upto += 1;
            }
        }

        Ok(())
    }
    pub(crate) fn do_next<FB>(&mut self, sub: &mut FB) -> Result<()>
    where
        FB: FSTEnumBase<O, F>,
    {
        if self.upto == 0 {
            self.upto = 1;
            let (left, right) = self.arcs.split_at_mut(1);
            let arc0 = &left[0];
            let arc1 = &mut right[0];
            self.fst
                .read_first_target_arc(arc0, arc1, &mut self.fst_reader)?;
        } else {
            while self.arcs[self.upto].is_last() {
                self.upto -= 1;
                if self.upto == 0 {
                    return Ok(());
                }
            }

            let arc = &mut self.arcs[self.upto];
            self.fst.read_next_arc(arc, &mut self.fst_reader)?;
        }
        self.push_first(sub)?;
        Ok(())
    }
    // TODO: should we return a status here (SEEK_FOUND / SEEK_NOT_FOUND /
    // SEEK_END)?  saves the eq check above?
    /// Seeks to smallest term that's &gt;= target.
    pub(crate) fn do_seek_ceil<FB>(&mut self, sub: &mut FB, target: &FB::V) -> Result<()>
    where
        FB: FSTEnumBase<O, F>,
    {
        // TODO: possibly caller could/should provide common
        // prefix length?  ie this work may be redundant if
        // caller is in fact intersecting against its own automaton

        // Save time by starting at the end of the shared prefix
        // between current term & the target:
        self.rewind_prefix(sub, target)?;
        let mut upto = self.upto;

        let mut fst_reader = self.fst.get_bytes_reader()?;
        // Now scan forward, matching the new suffix of the target
        loop {
            let target_label = sub.get_target_label(self, target)?;
            let arc = &mut self.arcs[upto];

            if arc.bytes_per_arc() != 0 && arc.label() != END_LABEL {
                let node_flags = arc.node_flags();
                let result = match node_flags {
                    ARCS_FOR_DIRECT_ADDRESSING => {
                        match self.do_seek_ceil_array_direct_addressing(
                            upto,
                            target_label,
                            &mut fst_reader,
                            sub,
                        )? {
                            Some(index) => {
                                upto = index;
                                Some(())
                            },
                            None => None,
                        }
                    },
                    ARCS_FOR_BINARY_SEARCH => {
                        match self.do_seek_ceil_array_packed(
                            upto,
                            target_label,
                            &mut fst_reader,
                            sub,
                        )? {
                            Some(index) => {
                                upto = index;
                                Some(())
                            },
                            None => None,
                        }
                    },
                    ARCS_FOR_CONTINUOUS => {
                        match self.do_seek_ceil_array_continuous(
                            upto,
                            target_label,
                            &mut fst_reader,
                            sub,
                        )? {
                            Some(index) => {
                                upto = index;
                                Some(())
                            },
                            None => None,
                        }
                    },
                    _ => Some(()),
                };

                match result {
                    Some(_) => continue,
                    None => break,
                }
            } else {
                let result = self.do_seek_ceil_list(upto, target_label, sub)?;
                match result {
                    Some(index) => {
                        upto = index;
                    },
                    None => break,
                }
            }
        }

        Ok(())
    }

    fn do_seek_ceil_array_continuous<FB>(
        &mut self,
        arc_index: usize,
        target_label: i32,
        reader: &mut F::FstBytesReader,
        sub: &mut FB,
    ) -> Result<Option<usize>>
    where
        FB: FSTEnumBase<O, F>,
    {
        let arc = &mut self.arcs[arc_index];
        let target_index = target_label - arc.first_label();

        if target_index >= arc.num_arcs() {
            self.rollback_to_last_fork_then_push(sub)?;
            return Ok(None);
        }

        if target_index < 0 {
            self.fst.read_arc_by_continuous(arc, reader, 0)?;
            debug_assert!(arc.label() > target_label);
            self.push_first(sub)?;
            Ok(None)
        } else {
            {
                self.fst.read_arc_by_continuous(arc, reader, target_index)?;
                debug_assert_eq!(arc.label(), target_label);

                self.output[self.upto] = self
                    .fst
                    .outputs
                    .add(&self.output[self.upto - 1], &arc.output());

                if target_label == END_LABEL {
                    return Ok(None);
                }

                sub.set_current_label(arc.label(), self)?;
                self.incr(sub)?;
            }
            debug_assert!(self.upto > arc_index);
            let (left, right) = self.arcs.split_at_mut(self.upto);
            let arc = &left[arc_index];
            let next_arc = &mut right[0];
            self.fst
                .read_first_target_arc(arc, next_arc, &mut self.fst_reader)?;
            Ok(Some(self.upto))
        }
    }
    fn do_seek_ceil_array_direct_addressing<FB>(
        &mut self,
        arc_index: usize,
        target_label: i32,
        reader: &mut F::FstBytesReader,
        sub: &mut FB,
    ) -> Result<Option<usize>>
    where
        FB: FSTEnumBase<O, F>,
    {
        let arc = &mut self.arcs[arc_index];
        let mut target_index = target_label - arc.first_label();

        if target_index >= arc.num_arcs() {
            self.rollback_to_last_fork_then_push(sub)?;
            return Ok(None);
        }

        if target_index < 0 {
            target_index = -1;
        } else if BitTable::is_bit_set(target_index, arc, reader)? {
            {
                self.fst
                    .read_arc_by_direct_addressing(arc, reader, target_index)?;
                debug_assert_eq!(arc.label(), target_label);

                self.output[self.upto] = self
                    .fst
                    .outputs
                    .add(&self.output[self.upto - 1], &arc.output());

                if target_label == END_LABEL {
                    return Ok(None);
                }

                sub.set_current_label(arc.label(), self)?;
                self.incr(sub)?;
            }
            debug_assert!(self.upto > arc_index);
            let (left, right) = self.arcs.split_at_mut(self.upto);
            let arc = &left[arc_index];
            let next_arc = &mut right[0];
            self.fst
                .read_first_target_arc(arc, next_arc, &mut self.fst_reader)?;
            return Ok(Some(self.upto));
        }

        let ceil_index = BitTable::next_bit_set(target_index, arc, reader)?;
        debug_assert_ne!(ceil_index, -1);

        self.fst
            .read_arc_by_direct_addressing(arc, reader, ceil_index)?;
        debug_assert!(arc.label() > target_label);
        self.push_first(sub)?;
        Ok(None)
    }
    fn do_seek_ceil_array_packed<FB>(
        &mut self,
        arc_index: usize,
        target_label: i32,
        reader: &mut F::FstBytesReader,
        sub: &mut FB,
    ) -> Result<Option<usize>>
    where
        FB: FSTEnumBase<O, F>,
    {
        let arc = &mut self.arcs[arc_index];
        let mut idx = Util::binary_search(&self.fst, arc, target_label)?;

        if idx >= 0 {
            self.fst.read_arc_by_index(arc, reader, idx)?;
            debug_assert_eq!(arc.arc_idx(), idx);
            debug_assert_eq!(arc.label(), target_label);

            self.output[self.upto] = self
                .fst
                .outputs
                .add(&self.output[self.upto - 1], &arc.output());

            if target_label == END_LABEL {
                return Ok(None);
            }

            sub.set_current_label(arc.label(), self)?;
            self.incr(sub)?;
            debug_assert!(self.upto > arc_index);
            let (left, right) = self.arcs.split_at_mut(self.upto);
            let arc = &left[arc_index];
            let next_arc = &mut right[0];
            self.fst
                .read_first_target_arc(arc, next_arc, &mut self.fst_reader)?;
            return Ok(Some(self.upto));
        }

        idx = -1 - idx;
        if idx == arc.num_arcs() {
            self.fst.read_arc_by_index(arc, reader, idx - 1)?;
            debug_assert!(arc.is_last());

            if self.upto == 0 {
                return Ok(None);
            }
            // Dead end (target is after the last arc);
            // rollback to last fork then push
            if self.upto == 0 {
                return Err(LuceneError::illegal_state("upto should be greater than 0"));
            }
            self.upto -= 1;
            while self.upto > 0 {
                let prev_arc = &mut self.arcs[self.upto];
                if !prev_arc.is_last() {
                    self.fst.read_next_arc(prev_arc, &mut self.fst_reader)?;
                    self.push_first(sub)?;
                    return Ok(None);
                }
                self.upto -= 1;
            }
            Ok(None)
        } else {
            // Ceiling - arc with least higher label
            self.fst.read_arc_by_index(arc, reader, idx)?;
            debug_assert!(arc.label() > target_label);
            self.push_first(sub)?;
            Ok(None)
        }
    }
    fn do_seek_ceil_list<FB>(
        &mut self,
        arc_index: usize,
        target_label: i32,
        sub: &mut FB,
    ) -> Result<Option<usize>>
    where
        FB: FSTEnumBase<O, F>,
    {
        let upto = arc_index;
        let arc = &mut self.arcs[upto];
        if arc.label() == target_label {
            self.output[self.upto] = self
                .fst
                .outputs
                .add(&self.output[self.upto - 1], &arc.output());

            if target_label == END_LABEL {
                return Ok(None);
            }

            sub.set_current_label(arc.label(), self)?;
            self.incr(sub)?;
            debug_assert!(self.upto > arc_index);
            let (left, right) = self.arcs.split_at_mut(self.upto);
            let arc = &left[arc_index];
            let next_arc = &mut right[0];
            self.fst
                .read_first_target_arc(arc, next_arc, &mut self.fst_reader)?;
            Ok(Some(self.upto))
        } else if arc.label() > target_label {
            self.push_first(sub)?;
            Ok(None)
        } else if arc.is_last() {
            if self.upto == 0 {
                return Err(LuceneError::illegal_state("upto should be greater than 0"));
            }
            self.upto -= 1;
            while self.upto > 0 {
                let prev_arc = &mut self.arcs[self.upto];
                if !prev_arc.is_last() {
                    self.fst.read_next_arc(prev_arc, &mut self.fst_reader)?;
                    self.push_first(sub)?;
                    return Ok(None);
                }
                self.upto -= 1;
            }
            Ok(None)
        } else {
            self.fst.read_next_arc(arc, &mut self.fst_reader)?;
            Ok(Some(upto))
        }
    }
    pub(crate) fn do_seek_floor<FB>(&mut self, sub: &mut FB, target: &FB::V) -> Result<()>
    where
        FB: FSTEnumBase<O, F>,
    {
        // TODO: possibly caller could/should provide common
        // prefix length?  ie this work may be redundant if
        // caller is in fact intersecting against its own
        // automaton
        // System.out.println("FE: seek floor upto=" + upto);

        // Save CPU by starting at the end of the shared prefix
        // b/w our current term & the target:
        self.rewind_prefix(sub, target)?;
        let mut upto = self.upto;

        let mut fst_reader = self.fst.get_bytes_reader()?;

        loop {
            let target_label = sub.get_target_label(self, target)?;
            let arc = &mut self.arcs[upto];

            if arc.bytes_per_arc() != 0 && arc.label() != END_LABEL {
                let node_flags = arc.node_flags();
                let result = match node_flags {
                    ARCS_FOR_DIRECT_ADDRESSING => {
                        match self.do_seek_floor_array_direct_addressing(
                            upto,
                            target_label,
                            &mut fst_reader,
                            sub,
                            target,
                        )? {
                            Some(index) => {
                                upto = index;
                                Some(())
                            },
                            None => None,
                        }
                    },
                    ARCS_FOR_BINARY_SEARCH => {
                        match self.do_seek_floor_array_packed(
                            upto,
                            target_label,
                            &mut fst_reader,
                            sub,
                            target,
                        )? {
                            Some(index) => {
                                upto = index;
                                Some(())
                            },
                            None => None,
                        }
                    },
                    ARCS_FOR_CONTINUOUS => {
                        match self.do_seek_floor_continuous(
                            upto,
                            target_label,
                            &mut fst_reader,
                            sub,
                            target,
                        )? {
                            Some(index) => {
                                upto = index;
                                Some(())
                            },
                            None => None,
                        }
                    },
                    _ => Some(()),
                };

                match result {
                    Some(_) => {
                        continue;
                    },
                    None => break,
                }
            } else {
                let result = self.do_seek_floor_list(upto, target_label, sub, target)?;
                match result {
                    Some(index) => {
                        upto = index;
                    },
                    None => break,
                }
            }
        }

        Ok(())
    }
    fn do_seek_floor_continuous<FB>(
        &mut self,
        arc_index: usize,
        target_label: i32,
        reader: &mut F::FstBytesReader,
        sub: &mut FB,
        target: &FB::V,
    ) -> Result<Option<usize>>
    where
        FB: FSTEnumBase<O, F>,
    {
        let upto = arc_index;
        let arc = &mut self.arcs[upto];
        let target_index = target_label - arc.first_label();

        if target_index < 0 {
            let result =
                self.backtrack_to_floor_arc(arc_index, target_label, reader, sub, target)?;
            debug_assert!(result.is_none());
            Ok(None)
        } else if target_index >= arc.num_arcs() {
            self.fst.read_last_arc_by_continuous(arc, reader)?;
            debug_assert!(arc.label() < target_label);
            debug_assert!(arc.is_last());
            self.push_last(sub)?;
            Ok(None)
        } else {
            self.fst.read_arc_by_continuous(arc, reader, target_index)?;
            debug_assert_eq!(arc.label(), target_label);

            self.output[self.upto] = self
                .fst
                .outputs
                .add(&self.output[self.upto - 1], &arc.output());

            if target_label == END_LABEL {
                return Ok(None);
            }

            sub.set_current_label(arc.label(), self)?;
            self.incr(sub)?;
            let (left, right) = self.arcs.split_at_mut(self.upto);
            let arc = &left[arc_index];
            let next_arc = &mut right[0];
            self.fst
                .read_first_target_arc(arc, next_arc, &mut self.fst_reader)?;
            Ok(Some(self.upto))
        }
    }
    fn do_seek_floor_array_direct_addressing<FB>(
        &mut self,
        arc_index: usize,
        target_label: i32,
        reader: &mut F::FstBytesReader,
        sub: &mut FB,
        target: &FB::V,
    ) -> Result<Option<usize>>
    where
        FB: FSTEnumBase<O, F>,
    {
        let upto = arc_index;
        let arc = &mut self.arcs[upto];
        let target_index = target_label - arc.first_label();

        if target_index < 0 {
            let result = self.backtrack_to_floor_arc(upto, target_label, reader, sub, target)?;
            debug_assert!(result.is_none());
            Ok(None)
        } else if target_index >= arc.num_arcs() {
            self.fst.read_last_arc_by_direct_addressing(arc, reader)?;
            debug_assert!(arc.label() < target_label);
            debug_assert!(arc.is_last());
            self.push_last(sub)?;
            Ok(None)
        } else {
            if BitTable::is_bit_set(target_index, arc, reader)? {
                self.fst
                    .read_arc_by_direct_addressing(arc, reader, target_index)?;
                debug_assert_eq!(arc.label(), target_label);

                self.output[self.upto] = self
                    .fst
                    .outputs
                    .add(&self.output[self.upto - 1], &arc.output());

                if target_label == END_LABEL {
                    return Ok(None);
                }

                sub.set_current_label(arc.label(), self)?;
                self.incr(sub)?;
                let (left, right) = self.arcs.split_at_mut(self.upto);
                let arc = &left[arc_index];
                let next_arc = &mut right[0];
                self.fst
                    .read_first_target_arc(arc, next_arc, &mut self.fst_reader)?;
                return Ok(Some(self.upto));
            }
            // Scan backwards to find a floor arc.
            let floor_index = BitTable::previous_bit_set(target_index, arc, reader)?;
            debug_assert_ne!(floor_index, -1);

            self.fst
                .read_arc_by_direct_addressing(arc, reader, floor_index)?;
            debug_assert!(arc.label() < target_label);
            debug_assert!(
                arc.is_last() || self.fst.read_next_arc_label(arc, reader)? > target_label
            );
            self.push_last(sub)?;
            Ok(None)
        }
    }
    /// Target is beyond the last arc, out of label range. Dead end (target is
    /// after the last arc); rollback to last fork then push.
    fn rollback_to_last_fork_then_push<FB>(&mut self, sub: &mut FB) -> Result<()>
    where
        FB: FSTEnumBase<O, F>,
    {
        if self.upto == 0 {
            return Err(LuceneError::illegal_state("upto should be greater than 0"));
        }
        self.upto -= 1;
        while self.upto > 0 {
            let upto = self.upto;
            let prev_arc = &mut self.arcs[upto];
            if !prev_arc.is_last() {
                self.fst.read_next_arc(prev_arc, &mut self.fst_reader)?;
                self.push_first(sub)?;
                return Ok(());
            }
            self.upto -= 1;
        }
        Ok(())
    }
    /// Backtracks until it finds a node whose first arc is before our target
    /// label. Then on that node, finds the arc just before the
    /// `target_label`.
    ///
    ///
    /// # Returns
    ///
    /// `None` to continue the seek floor recursion loop.
    fn backtrack_to_floor_arc<FB>(
        &mut self,
        arc_index: usize,
        mut target_label: i32,
        reader: &mut F::FstBytesReader,
        sub: &mut FB,
        target: &FB::V,
    ) -> Result<Option<Arc<O::V>>>
    where
        FB: FSTEnumBase<O, F>,
    {
        let mut upto = arc_index;
        loop {
            let (label, is_last, bytes_per_arc, node_flags) = {
                let (left, right) = self.arcs.split_at_mut(upto);
                let prev_arc = &left[self.upto - 1];
                let arc = &mut right[0];
                // First, walk backwards until we find a node which first arc is
                // before our target label.
                self.fst
                    .read_first_target_arc(prev_arc, arc, &mut self.fst_reader)?;
                (
                    arc.label(),
                    arc.is_last(),
                    arc.bytes_per_arc(),
                    arc.node_flags(),
                )
            };

            if label < target_label {
                if !is_last {
                    if bytes_per_arc != 0 && label != END_LABEL {
                        match node_flags {
                            ARCS_FOR_BINARY_SEARCH => {
                                self.find_next_floor_arc_binary_search(upto, target_label, reader)?;
                            },
                            ARCS_FOR_DIRECT_ADDRESSING => {
                                self.find_next_floor_arc_direct_addressing(
                                    upto,
                                    target_label,
                                    reader,
                                )?;
                            },
                            ARCS_FOR_CONTINUOUS => {
                                self.find_next_floor_arc_continuous(upto, target_label, reader)?;
                            },
                            _ => unreachable!(),
                        }
                    } else {
                        let arc = &mut self.arcs[upto];
                        while !is_last && self.fst.read_next_arc_label(arc, reader)? < target_label
                        {
                            self.fst.read_next_arc(arc, &mut self.fst_reader)?;
                        }
                    }
                }
                let arc = &mut self.arcs[upto];

                debug_assert!(arc.label() < target_label);
                debug_assert!(
                    arc.is_last() || self.fst.read_next_arc_label(arc, reader)? >= target_label
                );
                self.push_last(sub)?;
                return Ok(None);
            }

            self.upto -= 1;
            if self.upto == 0 {
                return Ok(None);
            }
            target_label = sub.get_target_label(self, target)?;
            upto = self.upto;
        }
    }
    /// Finds and reads an arc on the current node whose label is strictly less
    /// than the given label. Skips the first arc, finds the next floor arc;
    /// or none if the floor arc is the first arc itself (in this case it
    /// has already been read).
    ///
    ///
    /// Precondition: the given arc is the first arc of the node.
    fn find_next_floor_arc_direct_addressing(
        &mut self,
        arc_index: usize,
        target_label: i32,
        reader: &mut F::FstBytesReader,
    ) -> Result<()> {
        let arc = &mut self.arcs[arc_index];
        debug_assert_eq!(arc.node_flags(), ARCS_FOR_DIRECT_ADDRESSING);
        debug_assert_ne!(arc.label(), END_LABEL);
        debug_assert_eq!(arc.label(), arc.first_label());

        if arc.num_arcs() > 1 {
            let target_index = target_label - arc.first_label();
            debug_assert!(target_index >= 0);
            if target_index >= arc.num_arcs() {
                // Beyond last arc. Take last arc.
                self.fst.read_last_arc_by_direct_addressing(arc, reader)?;
            } else {
                // Take the preceding arc, even if the target is present.
                let floor_index = BitTable::previous_bit_set(target_index, arc, reader)?;
                if floor_index > 0 {
                    self.fst
                        .read_arc_by_direct_addressing(arc, reader, floor_index)?;
                }
            }
        }

        Ok(())
    }

    /// Same as [`find_next_floor_arc_direct_addressing`](Self::find_next_floor_arc_direct_addressing) for continuous node.
    fn find_next_floor_arc_continuous(
        &mut self,
        arc_index: usize,
        target_label: i32,
        reader: &mut F::FstBytesReader,
    ) -> Result<()> {
        let arc = &mut self.arcs[arc_index];
        debug_assert_eq!(arc.node_flags(), ARCS_FOR_CONTINUOUS);
        debug_assert_ne!(arc.label(), END_LABEL);
        debug_assert_eq!(arc.label(), arc.first_label());

        if arc.num_arcs() > 1 {
            let target_index = target_label - arc.first_label();
            debug_assert!(target_index >= 0);

            if target_index >= arc.num_arcs() {
                // Beyond last arc. Take last arc.
                self.fst.read_last_arc_by_continuous(arc, reader)?;
            } else {
                self.fst
                    .read_arc_by_continuous(arc, reader, target_index - 1)?;
            }
        }

        Ok(())
    }
    /// Same as [`find_next_floor_arc_direct_addressing`](Self::find_next_floor_arc_direct_addressing) for binary search node.
    fn find_next_floor_arc_binary_search(
        &mut self,
        arc_index: usize,
        target_label: i32,
        reader: &mut F::FstBytesReader,
    ) -> Result<()> {
        let arc = &mut self.arcs[arc_index];
        debug_assert_eq!(arc.node_flags(), ARCS_FOR_BINARY_SEARCH);
        debug_assert_ne!(arc.label(), END_LABEL);
        debug_assert_eq!(arc.arc_idx(), 0);

        if arc.num_arcs() > 1 {
            let idx = Util::binary_search(&self.fst, arc, target_label)?;
            debug_assert_ne!(idx, -1);
            if idx > 1 {
                self.fst.read_arc_by_index(arc, reader, idx - 1)?;
            } else if idx < -2 {
                self.fst.read_arc_by_index(arc, reader, -2 - idx)?;
            }
        }

        Ok(())
    }
    fn do_seek_floor_array_packed<FB>(
        &mut self,
        arc_index: usize,
        target_label: i32,
        reader: &mut F::FstBytesReader,
        sub: &mut FB,
        target: &FB::V,
    ) -> Result<Option<usize>>
    where
        FB: FSTEnumBase<O, F>,
    {
        let upto = arc_index;
        let arc = &mut self.arcs[upto];
        let idx = Util::binary_search(&self.fst, arc, target_label)?;

        if idx >= 0 {
            self.fst.read_arc_by_index(arc, reader, idx)?;
            debug_assert_eq!(arc.arc_idx(), idx);
            debug_assert_eq!(
                arc.label(),
                target_label,
                "arc.label()={} vs target_label={} mid={}",
                arc.label(),
                target_label,
                idx
            );
            self.output[self.upto] = self
                .fst
                .outputs
                .add(&self.output[self.upto - 1], &arc.output());

            if target_label == END_LABEL {
                return Ok(None);
            }

            sub.set_current_label(arc.label(), self)?;
            self.incr(sub)?;
            let (left, right) = self.arcs.split_at_mut(self.upto);
            let arc = &left[arc_index];
            let next_arc = &mut right[0];
            self.fst
                .read_first_target_arc(arc, next_arc, &mut self.fst_reader)?;
            Ok(Some(self.upto))
        } else if idx == -1 {
            let result = self.backtrack_to_floor_arc(upto, target_label, reader, sub, target)?;
            debug_assert!(result.is_none());
            Ok(None)
        } else {
            let floor_idx = -2 - idx;
            self.fst.read_arc_by_index(arc, reader, floor_idx)?;
            debug_assert!(
                arc.is_last() || self.fst.read_next_arc_label(arc, reader)? > target_label
            );
            debug_assert!(
                arc.label() < target_label,
                "arc.label()={} vs target_label={}",
                arc.label(),
                target_label
            );
            self.push_last(sub)?;
            Ok(None)
        }
    }
    fn do_seek_floor_list<FB>(
        &mut self,
        arc_index: usize,
        mut target_label: i32,
        sub: &mut FB,
        target: &FB::V,
    ) -> Result<Option<usize>>
    where
        FB: FSTEnumBase<O, F>,
    {
        let upto = arc_index;
        let arc = &mut self.arcs[upto];
        if arc.label() == target_label {
            self.output[self.upto] = self
                .fst
                .outputs
                .add(&self.output[self.upto - 1], &arc.output());

            if target_label == END_LABEL {
                return Ok(None);
            }

            sub.set_current_label(arc.label(), self)?;
            self.incr(sub)?;
            let (left, right) = self.arcs.split_at_mut(self.upto);
            let arc = &left[arc_index];
            let next_arc = &mut right[0];
            self.fst
                .read_first_target_arc(arc, next_arc, &mut self.fst_reader)?;
            Ok(Some(self.upto))
        } else if arc.label() > target_label {
            let mut upto = upto;
            // TODO: if each arc could somehow read the arc just
            // before, we can save this re-scan.  The ceil case
            // doesn't need this because it reads the next arc
            // instead:
            loop {
                let (left, right) = self.arcs.split_at_mut(upto);
                let prev_arc = &left[self.upto - 1];
                let arc = &mut right[0];
                // First, walk backwards until we find a first arc
                // that's before our target label:
                self.fst
                    .read_first_target_arc(prev_arc, arc, &mut self.fst_reader)?;
                if arc.label() < target_label {
                    // Then, scan forwards to the arc just before
                    // the targetLabel:
                    while !arc.is_last()
                        && self.fst.read_next_arc_label(arc, &mut self.fst_reader)? < target_label
                    {
                        self.fst.read_next_arc(arc, &mut self.fst_reader)?;
                    }
                    self.push_last(sub)?;
                    return Ok(None);
                }

                self.upto -= 1;
                if self.upto == 0 {
                    return Ok(None);
                }
                target_label = sub.get_target_label(self, target)?;
                upto = self.upto;
            }
        } else if !arc.is_last() {
            let next_label = self.fst.read_next_arc_label(arc, &mut self.fst_reader)?;
            if next_label > target_label {
                self.push_last(sub)?;
                Ok(None)
            } else {
                self.fst.read_next_arc(arc, &mut self.fst_reader)?;
                Ok(Some(upto))
            }
        } else {
            self.push_last(sub)?;
            Ok(None)
        }
    }
    pub(crate) fn do_seek_exact<FB>(&mut self, sub: &mut FB, target: &FB::V) -> Result<bool>
    where
        FB: FSTEnumBase<O, F>,
    {
        // TODO: possibly caller could/should provide common
        // prefix length?  ie this work may be redundant if
        // caller is in fact intersecting against its own
        // automaton
        // Save time by starting at the end of the shared prefix
        // b/w our current term & the target:
        self.rewind_prefix(sub, target)?;
        let mut upto = self.upto - 1;
        let mut target_label = sub.get_target_label(self, target)?;
        let mut fst_reader = self.fst.get_bytes_reader()?;
        loop {
            let (left, right) = self.arcs.split_at_mut(self.upto);
            let arc = &left[upto];
            let next_arc = &mut right[0];

            let found = self
                .fst
                .find_target_arc(target_label, arc, next_arc, &mut fst_reader)?;

            if found.is_none() {
                // fallback: reset to first arc for correct state
                self.fst
                    .read_first_target_arc(arc, next_arc, &mut fst_reader)?;
                return Ok(false);
            }

            self.output[self.upto] = self
                .fst
                .outputs
                .add(&self.output[self.upto - 1], &next_arc.output());

            if target_label == END_LABEL {
                return Ok(true);
            }

            sub.set_current_label(target_label, self)?;
            upto = self.upto;
            self.incr(sub)?;
            target_label = sub.get_target_label(self, target)?;
        }
    }
    fn incr<FB>(&mut self, sub: &mut FB) -> Result<()>
    where
        FB: FSTEnumBase<O, F>,
    {
        self.upto += 1;
        sub.grow(self)?;
        debug_assert!(self.upto <= i32::MAX as usize);
        if self.arcs.len() <= self.upto {
            ArrayUtil::grow_with_len(&mut self.arcs, self.upto + 1);
        }

        if self.output.len() <= self.upto {
            ArrayUtil::grow_with_len(&mut self.output, self.upto + 1);
        }
        Ok(())
    }
    // Appends current arc, and then recurses from its target,
    // appending first arc all the way to the final node
    fn push_first<FB>(&mut self, sub: &mut FB) -> Result<()>
    where
        FB: FSTEnumBase<O, F>,
    {
        let mut upto = self.upto;
        let (mut output, mut label) = {
            let arc = &mut self.arcs[upto];
            (arc.output(), arc.label())
        };
        loop {
            self.output[self.upto] = self.fst.outputs.add(&self.output[self.upto - 1], &output);

            if label == END_LABEL {
                break;
            }
            sub.set_current_label(label, self)?;
            upto = self.upto;
            self.incr(sub)?;
            let (left, right) = self.arcs.split_at_mut(self.upto);
            let arc = &left[upto];
            let next_arc = &mut right[0];
            self.fst
                .read_first_target_arc(arc, next_arc, &mut self.fst_reader)?;
            output = next_arc.output();
            label = next_arc.label();
        }
        Ok(())
    }
    // Recurses from current arc, appending last arc all the
    // way to the first final node
    fn push_last<FB>(&mut self, sub: &mut FB) -> Result<()>
    where
        FB: FSTEnumBase<O, F>,
    {
        let mut upto = self.upto;
        let (mut output, mut label) = {
            let arc = &mut self.arcs[upto];
            (arc.output(), arc.label())
        };
        loop {
            sub.set_current_label(label, self)?;
            self.output[self.upto] = self.fst.outputs.add(&self.output[self.upto - 1], &output);

            if label == END_LABEL {
                break;
            }
            upto = self.upto;
            self.incr(sub)?;

            let (left, right) = self.arcs.split_at_mut(self.upto);
            let arc = &left[upto];
            let next_arc = &mut right[0];
            self.fst
                .read_last_target_arc(arc, next_arc, &mut self.fst_reader)?;
            output = next_arc.output();
            label = next_arc.label();
        }

        Ok(())
    }
}
pub(crate) trait FSTEnumBase<O, F>
where
    O: Outputs,
    F: FstReader,
{
    type V;
    fn get_target_label(&self, base: &FSTEnum<O, F>, target: &Self::V) -> Result<i32>;
    fn get_current_label(&self, base: &FSTEnum<O, F>) -> Result<i32>;
    fn set_current_label(&mut self, label: i32, base: &FSTEnum<O, F>) -> Result<()>;
    fn grow(&mut self, base: &FSTEnum<O, F>) -> Result<()>;
}
/// Holds a single input + output pair
#[derive(Clone)]
pub struct InputOutput<T, I> {
    pub(crate) input: I,
    pub(crate) output: T,
}
