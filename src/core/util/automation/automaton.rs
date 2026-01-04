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
use std::collections::BTreeSet;
use std::fmt;

use bit_set::BitSet;
use num_traits::ToPrimitive;

use crate::core::util::accountable::Accountable;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::automation::transition::Transition;
use crate::core::util::automation::transition_accessor::TransitionAccessor;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::in_place_merge_sorter::InPlaceMergeSorter;
use crate::core::util::{BitSetExt, SliceCopyOps, Sorter};

/// Struct representing an automaton and all its states and transitions. States
/// are integers and must be created using
/// [`create_state`](Automaton::create_state). Mark a state as an accept state
/// using [`set_accept`](Automaton::set_accept). Add transitions using
/// [`add_transition`](Automaton::add_transition).
///
/// Each state must have all of its transitions added at once; if this is too
/// restrictive, use [`Builder`] instead. State `0` is always the
/// initial state.
///
/// Once a state is finished—either because you've started adding transitions to
/// another state or you call [`finish_state`](Automaton::finish_state)—then
/// that state's transitions are:
/// - sorted (by min, then max, then dest)
/// - and reduced (transitions with adjacent labels going to the same
///   destination are combined).
#[derive(Debug, Clone)]
pub struct Automaton {
    /// Index into the `Vec<i32>` state array where we next write; this
    /// increments by 2 for each added state because we pack a pointer to
    /// the transitions array and a count of how many transitions
    /// leave the state.
    next_state: i32,
    /// Index into the `Vec<i32>` transitions array where we next write; this
    /// increments by 3 for each added transition because we pack `min`,
    /// `max`, and `dest` in sequence.
    next_transition: i32,
    /// The current state to which we are adding transitions. The caller must
    /// add all transitions for this state before moving on to another
    /// state.
    cur_state: i32,
    /// Index in the transitions array where this state's outgoing transitions
    /// are stored, or `-1` if this state has not added any transitions yet.
    /// Followed by the number of transitions.
    states: Vec<i32>,
    is_accept: BitSet,
    transitions: Vec<i32>,
    /// True if no state has two transitions leaving with the same label.
    deterministic: bool,
}

impl Default for Automaton {
    fn default() -> Self {
        Self::new()
    }
}

impl Automaton {
    pub fn new() -> Self {
        Self::with_capacity(2, 2)
    }
    /// Constructor which creates an automaton with enough space for the given
    /// number of states and transitions.
    ///
    /// Parameters:
    /// - `num_states`: Number of states
    /// - `num_transitions`: Number of transitions
    pub fn with_capacity(num_states: usize, num_transitions: usize) -> Self {
        Automaton {
            next_state: 0,
            next_transition: 0,
            cur_state: -1,
            states: vec![0; num_states * 2],
            is_accept: BitSet::with_capacity(num_states),
            transitions: vec![0; num_transitions * 3],
            deterministic: true,
        }
    }
    pub fn create_state(&mut self) -> i32 {
        self.grow_states();
        let state = self.next_state / 2;
        self.states[self.next_state as usize] = -1;
        self.next_state += 2;
        state
    }
    /// Set or clear this state as an accept state.
    pub fn set_accept(&mut self, state: i32, accept: bool) {
        debug_assert!(
            (0..self.get_num_states() as usize).contains(&(state as usize)),
            "state {state} out of bounds"
        );
        if accept {
            self.is_accept.insert(state as usize);
        } else {
            self.is_accept.remove(state as usize);
        }
    }
    /// Convenience method to get all transitions for all states. This is
    /// object-heavy; it's better to iterate state by state instead.
    pub fn get_sorted_transitions(&self) -> Vec<Vec<Transition>> {
        let num_states = self.get_num_states();
        let mut result = Vec::with_capacity(num_states as usize);
        for s in 0..num_states {
            let cnt = self.get_num_transitions_with_state(s) as usize;
            let mut row = Vec::with_capacity(cnt);
            for i in 0..cnt {
                let mut t = Transition::default();
                self.get_transition(s, i as i32, &mut t);
                row.push(t);
            }
            result.push(row);
        }
        result
    }
    /// Returns accept states. If the bit is set, then that state is an accept
    /// state.
    pub(crate) fn get_accept_states(&self) -> &BitSet {
        &self.is_accept
    }
    /// Returns `true` if this state is an accept state.
    pub fn is_accept(&self, state: i32) -> bool {
        self.is_accept.contains(state as usize)
    }
    /// Add a new transition with `min = max = label`.
    pub fn add_transition_label(&mut self, source: i32, dest: i32, label: i32) -> Result<()> {
        self.add_transition(source, dest, label, label)
    }
    /// Add a new transition with the specified `source`, `dest`, `min`, and
    /// `max`.
    pub fn add_transition(&mut self, source: i32, dest: i32, min: i32, max: i32) -> Result<()> {
        debug_assert!(self.next_transition % 3 == 0);
        let bounds = self.next_state / 2;
        debug_assert!((0..bounds).contains(&source));
        debug_assert!((0..bounds).contains(&dest));

        self.grow_transitions();

        if self.cur_state != source {
            if self.cur_state != -1 {
                self.finish_current_state()?;
            }
            self.cur_state = source;
            let source = source as usize;
            if self.states[2 * source] != -1 {
                return Err(LuceneError::illegal_state(format!(
                    "from state ({source}) already had transitions added"
                )));
            }
            debug_assert!(self.states[2 * source + 1] == 0);
            self.states[2 * source] = self.next_transition;
        }

        let next_transition = self.next_transition as usize;
        self.transitions[next_transition] = dest;
        self.transitions[next_transition + 1] = min;
        self.transitions[next_transition + 2] = max;
        self.next_transition += 3;
        // Increment transition count for this state
        self.states[2 * self.cur_state as usize + 1] += 1;
        Ok(())
    }
    /// Add a `virtual` epsilon transition between `source` and `dest`.
    /// The destination state must already have all transitions added,
    /// because this method simply copies those same transitions over to the
    /// source.
    pub fn add_epsilon(&mut self, source: i32, dest: i32) -> Result<()> {
        let mut t = Transition::default();
        let count = self.init_transition(dest, &mut t);
        for _ in 0..count {
            self.get_next_transition(&mut t);
            self.add_transition(source, t.dest, t.min, t.max)?;
        }
        if self.is_accept(dest) {
            self.set_accept(source, true);
        }
        Ok(())
    }
    /// Copies over all states and transitions from another automaton.  
    /// The state numbers are sequentially assigned (appended).
    pub fn copy(&mut self, other: &Automaton) {
        // Bulk copy and fix up state pointers
        let state_offset = self.get_num_states();
        let total_states = self.next_state + other.next_state;
        ArrayUtil::grow_with_len(&mut self.states, total_states as usize);
        self.states.copy_from(
            &other.states[0..other.next_state as usize],
            self.next_state as usize,
        );

        let next_state = self.next_state as usize;
        for i in (0..other.next_state as usize).step_by(2) {
            if self.states[next_state + i] != -1 {
                self.states[next_state + i] += self.next_transition;
            }
        }
        self.next_state += other.next_state;

        let other_num_states = other.get_num_states();
        let other_accept_states = other.get_accept_states();
        let mut state = 0;
        while state < other_num_states {
            state = other_accept_states.next_set_bit(state as usize);
            if state == -1 {
                break;
            }
            self.set_accept(state_offset + state, true);
            state += 1;
        }

        // Bulk copy and then fixup dest for each transition:
        let len = self.next_transition + other.next_transition;
        ArrayUtil::grow_with_len(&mut self.transitions, len as usize);
        self.transitions.copy_from(
            &other.transitions[0..other.next_transition as usize],
            self.next_transition as usize,
        );

        let next_transition = self.next_transition as usize;
        for i in (0..other.next_transition as usize).step_by(3) {
            self.transitions[next_transition + i] += state_offset;
        }
        self.next_transition += other.next_transition;

        if !other.deterministic {
            self.deterministic = false;
        }
    }
    /// Freezes the last state, sorting and reducing its transitions.
    fn finish_current_state(&mut self) -> Result<()> {
        let state = self.cur_state as usize;
        let num_transitions = self.states[2 * state + 1];
        debug_assert!(num_transitions > 0, "no transitions to finish");

        let offset = self.states[2 * state];
        let start = offset / 3;
        // sort by dest, then min, then max
        let sub = DestMinMaxSorter {
            transitions: &mut self.transitions,
        };
        let mut sort = InPlaceMergeSorter::new(sub);
        sort.sort(start, start + num_transitions)?;

        // merge adjacent transitions
        let mut upto = 0;
        let mut min = -1;
        let mut max = -1;
        let mut dest = -1;

        let offset = offset as usize;
        for i in 0..num_transitions as usize {
            let base = offset + 3 * i;
            let t_dest = self.transitions[base];
            let t_min = self.transitions[base + 1];
            let t_max = self.transitions[base + 2];

            if dest == t_dest {
                if t_min <= max + 1 {
                    if t_max > max {
                        max = t_max;
                    }
                } else {
                    if dest != -1 {
                        self.transitions[offset + 3 * upto] = dest;
                        self.transitions[offset + 3 * upto + 1] = min;
                        self.transitions[offset + 3 * upto + 2] = max;
                        upto += 1;
                    }

                    min = t_min;
                    max = t_max;
                }
            } else {
                if dest != -1 {
                    self.transitions[offset + 3 * upto] = dest;
                    self.transitions[offset + 3 * upto + 1] = min;
                    self.transitions[offset + 3 * upto + 2] = max;
                    upto += 1;
                }
                dest = t_dest;
                min = t_min;
                max = t_max;
            }
        }
        // flush last
        if dest != -1 {
            self.transitions[offset + 3 * upto] = dest;
            self.transitions[offset + 3 * upto + 1] = min;
            self.transitions[offset + 3 * upto + 2] = max;
            upto += 1;
        }

        // adjust counters
        debug_assert!(upto.to_i32().is_some());
        self.next_transition -= (num_transitions - upto as i32) * 3;
        self.states[2 * state + 1] = upto as i32;

        // Sort transitions by min/max/dest:
        let sub = MinMaxDestSorter {
            transitions: &mut self.transitions,
        };
        let mut sort = InPlaceMergeSorter::new(sub);
        sort.sort(start, start + upto as i32)?;

        // check determinism
        if self.deterministic && upto > 1 {
            let mut last_max = self.transitions[offset + 2];
            for i in 1..upto {
                let next_min = self.transitions[offset + 3 * i + 1];
                if next_min <= last_max {
                    self.deterministic = false;
                    break;
                }
                last_max = self.transitions[offset + 3 * i + 2];
            }
        }
        Ok(())
    }
    /// Returns `true` if this automaton is deterministic (for every state there
    /// is only one transition for each label).
    pub fn is_deterministic(&self) -> bool {
        self.deterministic
    }
    /// Finishes the current state; call this once you are done adding
    /// transitions for a state. This is automatically called when you start
    /// adding transitions to a new source state, but for the last state you
    /// add, you need to call this method manually.
    pub fn finish_state(&mut self) -> Result<()> {
        if self.cur_state != -1 {
            self.finish_current_state()?;
            self.cur_state = -1;
        }
        Ok(())
    }
    /// Returns how many states this automaton has.
    pub fn get_num_states(&self) -> i32 {
        self.next_state / 2
    }
    /// Returns how many transitions this automaton has.
    pub fn get_num_transitions(&self) -> i32 {
        self.next_transition / 3
    }
    fn grow_states(&mut self) {
        let len = (self.next_state + 2) as usize;
        if len > self.states.len() {
            ArrayUtil::grow_with_len(&mut self.states, len);
        }
    }

    fn grow_transitions(&mut self) {
        let len = (self.next_transition + 3) as usize;
        if len > self.transitions.len() {
            ArrayUtil::grow_with_len(&mut self.transitions, len);
        }
    }

    fn transition_sorted(&self, t: &Transition) -> bool {
        let upto = t.transition_upto;
        // Transition isn't initialized yet (this is the first transition)
        if upto == self.states[2 * t.source as usize] {
            return true;
        }
        let upto = upto as usize;

        let next_dest = self.transitions[upto];
        let next_min = self.transitions[upto + 1];
        let next_max = self.transitions[upto + 2];

        if next_min > t.min {
            true
        } else if next_min < t.min {
            false
        } else if next_max > t.max {
            true
        } else if next_max < t.max {
            false
        } else if next_dest > t.dest {
            true
        } else {
            // We should never see fully equal transitions here:
            false
        }
    }
    /// Returns sorted array of all interval start points.
    pub fn get_start_points(&self) -> Vec<i32> {
        let mut pointset = BTreeSet::new();
        pointset.insert(0);

        for s in (0..self.next_state as usize).step_by(2) {
            let mut trans = self.states[s] as usize;
            let limit = trans + 3 * self.states[s + 1] as usize;

            while trans < limit {
                let min = self.transitions[trans + 1];
                let max = self.transitions[trans + 2];
                pointset.insert(min);
                if max < char::MAX as i32 {
                    pointset.insert(max + 1);
                }
                trans += 3;
            }
        }
        pointset.into_iter().collect()
    }
    /// Performs lookup in transitions, assuming determinism.
    ///
    /// Parameters:
    /// - `state`: starting state
    /// - `label`: codepoint to look up
    ///
    /// Returns:
    /// - destination state, or `-1` if no matching outgoing transition
    pub fn step(&self, state: i32, label: i32) -> i32 {
        self.next_impl(state, 0, label, None)
    }
    /// Looks for the next transition that matches the provided label, assuming
    /// determinism.
    ///
    /// This method is similar to [`step(state, label)`](Automaton::step), but
    /// is used more efficiently when iterating over multiple transitions
    /// from the same source state. It keeps the latest reached transition
    /// index in `transition.transition_upto`, so the next call to this method
    /// can continue from there instead of restarting from the first
    /// transition.
    ///
    /// Parameters:
    /// - `transition`: The transition to start the lookup from (inclusive,
    ///   using its `source` and `transition_upto`). It is updated with the
    ///   matched transition; or with `dest = -1` if no match.
    /// - `label`: The codepoint to look up.
    ///
    /// Returns:
    /// - The destination state, or `-1` if no matching outgoing transition.
    pub fn next(&self, transition: &mut Transition, label: i32) -> i32 {
        self.next_impl(
            transition.source,
            transition.transition_upto,
            label,
            Some(transition),
        )
    }
    /// Looks for the next transition that matches the provided label, assuming
    /// determinism.
    ///
    /// Parameters:
    /// - `state`: The source state
    /// - `from_transition_index`: The transition index to start the lookup from
    ///   (inclusive); negative values are interpreted as `0`
    /// - `label`: The codepoint to look up
    /// - `transition`: The output transition to update with the matching
    ///   transition, or `None` for no update
    ///
    /// Returns:
    /// - The destination state, or `-1` if no matching outgoing transition.
    fn next_impl(
        &self,
        state: i32,
        from_transition_idx: i32,
        label: i32,
        transition: Option<&mut Transition>,
    ) -> i32 {
        debug_assert!(label >= 0);

        let state_index = 2 * state as usize;
        let first_transition = self.states[state_index];
        let num_transitions = self.states[state_index + 1];

        let mut low = from_transition_idx.max(0);
        let mut high = num_transitions - 1;
        // Since transitions are sorted,
        // binary search the transition for which label is within [minLabel, maxLabel].
        while low <= high {
            let mid = ((low + high) as u32 >> 1) as i32;
            let transition_index = (first_transition + 3 * mid) as usize;
            let min_label = self.transitions[transition_index + 1];
            if min_label > label {
                high = mid - 1;
            } else {
                let max_label = self.transitions[transition_index + 2];
                if max_label < label {
                    low = mid + 1;
                } else {
                    let dest = self.transitions[transition_index];
                    if let Some(tr) = transition {
                        tr.dest = dest;
                        tr.min = min_label;
                        tr.max = max_label;
                        tr.transition_upto = mid;
                    }
                    return dest;
                }
            }
        }

        let dest_state = -1;
        if let Some(tr) = transition {
            tr.dest = dest_state;
            tr.transition_upto = low;
        }
        dest_state
    }
    pub fn append_char_string(c: i32, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if (0x21..=0x7e).contains(&c) && c != '\\' as i32 && c != '"' as i32 {
            write!(f, "{}", char::from_u32(c as u32).unwrap_or('?'))
        } else {
            write!(f, "\\\\U{c:08X}")
        }
    }
}
impl TransitionAccessor for Automaton {
    fn init_transition(&self, state: i32, t: &mut Transition) -> i32 {
        debug_assert!(
            state < self.next_state / 2,
            "state {} next_state {}",
            state,
            self.next_state
        );
        t.source = state;
        t.transition_upto = self.states[2 * state as usize];
        self.get_num_transitions_with_state(state)
    }

    fn get_next_transition(&self, t: &mut Transition) {
        // Make sure there is still a transition left:
        debug_assert!(
            (t.transition_upto + 3 - self.states[2 * t.source as usize])
                <= 3 * self.states[2 * t.source as usize + 1]
        );
        // Make sure transitions are in fact sorted:
        debug_assert!(self.transition_sorted(t));
        debug_assert!(t.transition_upto >= 0);
        let base = t.transition_upto as usize;
        t.dest = self.transitions[base];
        t.min = self.transitions[base + 1];
        t.max = self.transitions[base + 2];
        t.transition_upto += 3;
    }

    fn get_num_transitions_with_state(&self, state: i32) -> i32 {
        debug_assert!(state >= 0);
        debug_assert!(state < self.get_num_states());
        let count = self.states[2 * state as usize + 1];
        if count == -1 { 0 } else { count }
    }

    fn get_transition(&self, state: i32, index: i32, t: &mut Transition) {
        let base = self.states[2 * state as usize] as usize;
        let offset = base + 3 * index as usize;
        t.source = state;
        t.dest = self.transitions[offset];
        t.min = self.transitions[offset + 1];
        t.max = self.transitions[offset + 2];
    }
}

impl Accountable for Automaton {
    fn ram_bytes_used(&self) -> Result<i64> {
        todo!()
    }
}
/// Records new states and transitions, and then [`finish`](Builder::finish)
/// creates the [`Automaton`]. Use this when you cannot create the automaton
/// directly because it's too restrictive to have to add all transitions leaving
/// each state at once.
pub struct Builder {
    next_state: i32,
    is_accept: BitSet,
    transitions: Vec<i32>,
    next_transition: i32,
}
impl Default for Builder {
    fn default() -> Self {
        Self::new()
    }
}

impl Builder {
    pub fn new() -> Self {
        Self::with_capacity(16, 16)
    }
    /// Constructor which creates a builder with enough space for the given
    /// number of states and transitions.
    ///
    /// Parameters:
    /// - `num_states`: Number of states
    /// - `num_transitions`: Number of transitions
    pub fn with_capacity(num_states: usize, num_transitions: usize) -> Self {
        let is_accept = BitSet::with_capacity(num_states);
        let transitions = vec![0; num_transitions * 4];
        Builder {
            next_state: 0,
            is_accept,
            transitions,
            next_transition: 0,
        }
    }
    /// Add a new transition with min = max = label.
    pub fn add_transition_label(&mut self, source: i32, dest: i32, label: i32) {
        self.add_transition(source, dest, label, label)
    }
    /// Add a new transition with the specified source, dest, min, max.
    pub fn add_transition(&mut self, source: i32, dest: i32, min: i32, max: i32) {
        let new_len = (self.next_transition + 4) as usize;
        if self.transitions.len() < new_len {
            ArrayUtil::grow_with_len(&mut self.transitions, new_len);
        }
        let mut next_transition = self.next_transition as usize;
        self.transitions[next_transition] = source;
        next_transition += 1;
        self.transitions[next_transition] = dest;
        next_transition += 1;
        self.transitions[next_transition] = min;
        next_transition += 1;
        self.transitions[next_transition] = max;
        next_transition += 1;
        self.next_transition = next_transition as i32;
    }
    /// Add a `virtual` epsilon transition between source and dest. Dest state
    /// must already have all transitions added because this method simply
    /// copies those same transitions over to source.
    pub fn add_epsilon(&mut self, source: i32, dest: i32) {
        let mut upto = 0;
        while upto < self.next_transition as usize {
            if self.transitions[upto] == dest {
                self.add_transition(
                    source,
                    self.transitions[upto + 1],
                    self.transitions[upto + 2],
                    self.transitions[upto + 3],
                );
            }
            upto += 4;
        }
        if self.is_accept(dest) {
            self.set_accept(source, true);
        }
    }
    /// Compiles all added states and transitions into a new [`Automaton`] and
    /// returns it.
    pub fn finish(&mut self) -> Result<Automaton> {
        let num_states = self.next_state;
        let num_transitions = self.next_transition / 4;
        let mut a = Automaton::with_capacity(num_states as usize, num_transitions as usize);

        for state in 0..num_states {
            a.create_state();
            a.set_accept(state, self.is_accept(state));
        }
        let sub = InPlaceMergeSorterImpl {
            transitions: &mut self.transitions,
        };
        let mut sort = InPlaceMergeSorter::new(sub);
        debug_assert!(num_transitions.to_i32().is_some());
        sort.sort(0, num_transitions)?;
        let mut upto = 0;
        while upto < self.next_transition as usize {
            a.add_transition(
                self.transitions[upto],
                self.transitions[upto + 1],
                self.transitions[upto + 2],
                self.transitions[upto + 3],
            )?;
            upto += 4;
        }

        a.finish_state()?;
        Ok(a)
    }
    /// Create a new state
    pub fn create_state(&mut self) -> i32 {
        let s = self.next_state;
        self.next_state += 1;
        s
    }

    /// Set or clear this state as an accept state.
    pub fn set_accept(&mut self, state: i32, accept: bool) {
        debug_assert!(
            (0..self.next_state).contains(&state),
            "state {state} out of bounds"
        );
        if accept {
            self.is_accept.insert(state as usize);
        } else {
            self.is_accept.remove(state as usize);
        }
    }

    /// Returns true if this state is an accept state.
    pub fn is_accept(&self, state: i32) -> bool {
        self.is_accept.contains(state as usize)
    }

    /// How many states this automaton has.
    pub fn get_num_states(&self) -> i32 {
        self.next_state
    }

    /// Copies over all states/transitions from other.
    pub fn copy(&mut self, other: &Automaton) {
        let offset = self.get_num_states();
        let other_num_states = other.get_num_states();

        // Copy all states
        self.copy_states(other);

        // Copy all transitions
        let mut t = Transition::default();
        for s in 0..other_num_states {
            let count = other.init_transition(s, &mut t);
            for _ in 0..count {
                other.get_next_transition(&mut t);
                self.add_transition(offset + s, offset + t.dest, t.min, t.max);
            }
        }
    }

    /// Copies over all states from other.
    pub fn copy_states(&mut self, other: &Automaton) {
        let other_num_states = other.get_num_states();
        for s in 0..other_num_states {
            let new_state = self.create_state();
            let is_accept = other.is_accept(s);
            self.set_accept(new_state, is_accept);
        }
    }
}

pub struct InPlaceMergeSorterImpl<'a> {
    transitions: &'a mut [i32],
}

impl<'a> InPlaceMergeSorterImpl<'a> {
    fn swap_one(&mut self, i: usize, j: usize) {
        self.transitions.swap(i, j);
    }
}
impl Sorter for InPlaceMergeSorterImpl<'_> {
    fn compare(&mut self, i: usize, j: usize) -> Result<i32> {
        let i_start = i * 4;
        let j_start = j * 4;

        // First src
        let i_src = self.transitions[i_start];
        let j_src = self.transitions[j_start];
        if i_src < j_src {
            return Ok(-1);
        }
        if i_src > j_src {
            return Ok(1);
        }

        // Then min
        let i_min = self.transitions[i_start + 2];
        let j_min = self.transitions[j_start + 2];
        if i_min < j_min {
            return Ok(-1);
        }
        if i_min > j_min {
            return Ok(1);
        }

        // Then max
        let i_max = self.transitions[i_start + 3];
        let j_max = self.transitions[j_start + 3];
        if i_max < j_max {
            return Ok(-1);
        }
        if i_max > j_max {
            return Ok(1);
        }

        // Finally dest
        let i_dest = self.transitions[i_start + 1];
        let j_dest = self.transitions[j_start + 1];
        if i_dest < j_dest {
            Ok(-1)
        } else if i_dest > j_dest {
            Ok(1)
        } else {
            Ok(0)
        }
    }

    fn swap(&mut self, i: usize, j: usize) -> Result<()> {
        let i_start = i * 4;
        let j_start = j * 4;
        self.swap_one(i_start, j_start);
        self.swap_one(i_start + 1, j_start + 1);
        self.swap_one(i_start + 2, j_start + 2);
        self.swap_one(i_start + 3, j_start + 3);
        Ok(())
    }
}
/// Sorts transitions by minimum label (ascending), then maximum label
/// (ascending), then destination (ascending).
pub struct MinMaxDestSorter<'a> {
    transitions: &'a mut [i32],
}
impl<'a> MinMaxDestSorter<'a> {
    fn swap_one(&mut self, i: usize, j: usize) {
        self.transitions.swap(i, j)
    }
}
impl Sorter for MinMaxDestSorter<'_> {
    fn compare(&mut self, i: usize, j: usize) -> Result<i32> {
        let i_start = 3 * i;
        let j_start = 3 * j;

        // First compare min
        let i_min = self.transitions[i_start + 1];
        let j_min = self.transitions[j_start + 1];
        if i_min < j_min {
            return Ok(-1);
        } else if i_min > j_min {
            return Ok(1);
        }

        // Then compare max
        let i_max = self.transitions[i_start + 2];
        let j_max = self.transitions[j_start + 2];
        if i_max < j_max {
            return Ok(-1);
        } else if i_max > j_max {
            return Ok(1);
        }

        // Finally compare dest
        let i_dest = self.transitions[i_start];
        let j_dest = self.transitions[j_start];
        if i_dest < j_dest {
            Ok(-1)
        } else if i_dest > j_dest {
            Ok(1)
        } else {
            Ok(0)
        }
    }

    fn swap(&mut self, i: usize, j: usize) -> Result<()> {
        let i_start = 3 * i;
        let j_start = 3 * j;
        self.swap_one(i_start, j_start);
        self.swap_one(i_start + 1, j_start + 1);
        self.swap_one(i_start + 2, j_start + 2);
        Ok(())
    }
}
/// Sorts transitions by destination (ascending), then by minimum label
/// (ascending), then by maximum label (ascending).
pub struct DestMinMaxSorter<'a> {
    transitions: &'a mut [i32],
}
impl<'a> DestMinMaxSorter<'a> {
    fn swap_one(&mut self, i: usize, j: usize) {
        self.transitions.swap(i, j)
    }
}
impl Sorter for DestMinMaxSorter<'_> {
    fn compare(&mut self, i: usize, j: usize) -> Result<i32> {
        let i_start = 3 * i;
        let j_start = 3 * j;

        // First dest:
        let i_dest = self.transitions[i_start];
        let j_dest = self.transitions[j_start];
        if i_dest < j_dest {
            return Ok(-1);
        } else if i_dest > j_dest {
            return Ok(1);
        }

        // Then min:
        let i_min = self.transitions[i_start + 1];
        let j_min = self.transitions[j_start + 1];
        if i_min < j_min {
            return Ok(-1);
        } else if i_min > j_min {
            return Ok(1);
        }

        // Then max:
        let i_max = self.transitions[i_start + 2];
        let j_max = self.transitions[j_start + 2];
        if i_max < j_max {
            return Ok(-1);
        } else if i_max > j_max {
            return Ok(1);
        }
        Ok(0)
    }

    fn swap(&mut self, i: usize, j: usize) -> Result<()> {
        let i_start = 3 * i;
        let j_start = 3 * j;
        self.swap_one(i_start, j_start);
        self.swap_one(i_start + 1, j_start + 1);
        self.swap_one(i_start + 2, j_start + 2);
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::collections::{BTreeSet, HashSet};

    use rand::Rng;
    use rand::prelude::SliceRandom;

    use crate::core::index::{BytesRef, BytesRefBuilder};
    use crate::core::util::ToInt;
    use crate::core::util::automation::automata::Automata;
    use crate::core::util::automation::automaton::{Automaton, Builder};
    use crate::core::util::automation::operations::Operations;
    use crate::core::util::automation::operations::tests::TestOperations;
    use crate::core::util::automation::reg_exp::RegExp;
    use crate::core::util::automation::transition::Transition;
    use crate::core::util::automation::transition_accessor::TransitionAccessor;
    use crate::core::util::automation::utf32_to_utf8::UTF32ToUTF8;
    use crate::core::util::error::lucene_error::{LuceneError, Result};
    use crate::core::util::fst_impl::util::Util;
    use crate::core::util::ints_ref::IntsRef;
    use crate::core::util::ints_ref_builder::IntsRefBuilder;
    use crate::core::util::unicode_util::UnicodeUtil;
    use crate::test::util::automaton::automaton_test_util::{
        AutomatonTestUtil, RandomAcceptedStrings,
    };
    use crate::test::util::automaton::minimization_operation::MinimizationOperations;
    use crate::test::util::lucene_test_case::lucene_test_case_util::{
        at_least, new_bytes_ref, new_bytes_ref_empty, new_bytes_ref_from_string, random,
        random_from_seed,
    };
    use crate::test::util::test_util::TestUtil;

    #[allow(dead_code)] // for quick search
    struct TestAutomaton;

    #[test]
    fn test_basic() -> Result<()> {
        let mut a = Automaton::new();
        let start = a.create_state();
        let x = a.create_state();
        let y = a.create_state();
        let end = a.create_state();
        a.set_accept(end, true);

        a.add_transition(start, x, 'a' as i32, 'a' as i32)?;
        a.add_transition(start, end, 'd' as i32, 'd' as i32)?;
        a.add_transition(x, y, 'b' as i32, 'b' as i32)?;
        a.add_transition(y, end, 'c' as i32, 'c' as i32)?;

        a.finish_state()?;
        Ok(())
    }
    #[test]
    fn test_reduce_basic() -> Result<()> {
        let mut a = Automaton::new();
        let start = a.create_state();
        let end = a.create_state();
        a.set_accept(end, true);

        // Should collapse to a-b:
        a.add_transition(start, end, 'a' as i32, 'a' as i32)?;
        a.add_transition(start, end, 'b' as i32, 'b' as i32)?;
        // Should collapse to m-m:
        a.add_transition(start, end, 'm' as i32, 'm' as i32)?;
        // Should collapse to x-y:
        a.add_transition(start, end, 'x' as i32, 'x' as i32)?;
        a.add_transition(start, end, 'y' as i32, 'y' as i32)?;

        a.finish_state()?;

        assert_eq!(3, a.get_num_transitions_with_state(start));

        let mut scratch = Transition::default();
        a.init_transition(start, &mut scratch);
        a.get_next_transition(&mut scratch);
        assert_eq!('a' as i32, scratch.min);
        assert_eq!('b' as i32, scratch.max);

        a.get_next_transition(&mut scratch);
        assert_eq!('m' as i32, scratch.min);
        assert_eq!('m' as i32, scratch.max);

        a.get_next_transition(&mut scratch);
        assert_eq!('x' as i32, scratch.min);
        assert_eq!('y' as i32, scratch.max);

        Ok(())
    }
    #[test]
    fn test_same_language() -> Result<()> {
        let a1 = Automata::make_string("foobar")?;
        let v = Operations::concatenate(
            &Automata::make_string("foo")?,
            &Automata::make_string("bar")?,
        )?;
        let a2 = Operations::remove_dead_states(&v)?;
        assert!(AutomatonTestUtil::same_language(&a1, &a2)?);
        Ok(())
    }
    #[test]
    fn test_common_prefix_string() -> Result<()> {
        let a = Operations::concatenate(
            &Automata::make_string("foobar")?,
            &Automata::make_any_string()?,
        )?;

        let prefix = Operations::get_common_prefix(&a)?;
        assert_eq!(prefix, "foobar");

        Ok(())
    }
    #[test]
    fn test_common_prefix_empty() -> Result<()> {
        let a = Automata::make_empty()?;
        let prefix = Operations::get_common_prefix(&a)?;
        assert_eq!(prefix, "");
        Ok(())
    }

    #[test]
    fn test_common_prefix_empty_string() -> Result<()> {
        let a = Automata::make_empty_string()?;
        let prefix = Operations::get_common_prefix(&a)?;
        assert_eq!(prefix, "");
        Ok(())
    }

    #[test]
    fn test_common_prefix_any() -> Result<()> {
        let a = Automata::make_any_string()?;
        let prefix = Operations::get_common_prefix(&a)?;
        assert_eq!(prefix, "");
        Ok(())
    }

    #[test]
    fn test_common_prefix_range() -> Result<()> {
        let a = Automata::make_char_range('a' as i32, 'b' as i32)?;
        let prefix = Operations::get_common_prefix(&a)?;
        assert_eq!(prefix, "");
        Ok(())
    }
    #[test]
    fn test_alternatives() -> Result<()> {
        let a = Automata::make_char('a' as i32)?;
        let c = Automata::make_char('c' as i32)?;
        let union = Operations::union(&a, &c)?;
        let prefix = Operations::get_common_prefix(&union)?;
        assert_eq!(prefix, "");
        Ok(())
    }

    #[test]
    fn test_common_prefix_leading_wildcard() -> Result<()> {
        let a =
            Operations::concatenate(&Automata::make_any_char()?, &Automata::make_string("boo")?)?;
        let prefix = Operations::get_common_prefix(&a)?;
        assert_eq!(prefix, "");
        Ok(())
    }

    #[test]
    fn test_common_prefix_trailing_wildcard() -> Result<()> {
        let a =
            Operations::concatenate(&Automata::make_string("boo")?, &Automata::make_any_char()?)?;
        let prefix = Operations::get_common_prefix(&a)?;
        assert_eq!(prefix, "boo");
        Ok(())
    }

    #[test]
    fn test_common_prefix_leading_kleen_star() -> Result<()> {
        let a = Operations::concatenate(
            &Automata::make_any_string()?,
            &Automata::make_string("boo")?,
        )?;
        let prefix = Operations::get_common_prefix(&a)?;
        assert_eq!(prefix, "");
        Ok(())
    }

    #[test]
    fn test_common_prefix_trailing_kleen_star() -> Result<()> {
        let a = Operations::concatenate(
            &Automata::make_string("boo")?,
            &Automata::make_any_string()?,
        )?;
        let prefix = Operations::get_common_prefix(&a)?;
        assert_eq!(prefix, "boo");
        Ok(())
    }
    #[test]
    fn test_common_prefix_dead_states() -> Result<()> {
        let a = Operations::concatenate(
            &Automata::make_any_string()?,
            &Automata::make_string("boo")?,
        )?;

        // reverse twice to create dead states
        let with_dead_states = Operations::reverse(&Operations::reverse(&a)?)?;

        let result = Operations::get_common_prefix(&with_dead_states);
        assert!(matches!(result, Err(LuceneError::IllegalArgument(_))));
        assert!(
            result
                .unwrap_err()
                .to_string()
                .eq("input automaton has dead states")
        );

        Ok(())
    }
    #[test]
    fn test_common_prefix_remove_dead_states() -> Result<()> {
        let a = Operations::concatenate(
            &Automata::make_any_string()?,
            &Automata::make_string("boo")?,
        )?;

        // reverse twice to create dead states
        let with_dead_states = Operations::reverse(&Operations::reverse(&a)?)?;

        // now remove the dead states
        let without_dead_states = Operations::remove_dead_states(&with_dead_states)?;

        let prefix = Operations::get_common_prefix(&without_dead_states)?;
        assert_eq!(prefix, "");

        Ok(())
    }
    #[test]
    fn test_common_prefix_optional() -> Result<()> {
        let mut a = Automaton::new();
        let init = a.create_state();
        let fini = a.create_state();
        a.set_accept(init, true);
        a.set_accept(fini, true);
        a.add_transition(init, fini, 'm' as i32, 'm' as i32)?;
        a.add_transition(fini, fini, 'm' as i32, 'm' as i32)?;
        a.finish_state()?;

        let prefix = Operations::get_common_prefix(&a)?;
        assert_eq!(prefix, "");

        Ok(())
    }

    #[test]
    fn test_common_prefix_nfa() -> Result<()> {
        let mut a = Automaton::new();
        let init = a.create_state();
        let medial = a.create_state();
        let fini = a.create_state();
        a.set_accept(fini, true);
        a.add_transition(init, medial, 'm' as i32, 'm' as i32)?;
        a.add_transition(init, fini, 'm' as i32, 'm' as i32)?;
        a.add_transition(medial, fini, 'o' as i32, 'o' as i32)?;
        a.finish_state()?;

        let prefix = Operations::get_common_prefix(&a)?;
        assert_eq!(prefix, "m");

        Ok(())
    }

    #[test]
    fn test_common_prefix_nfa_infinite() -> Result<()> {
        let mut a = Automaton::new();
        let init = a.create_state();
        let medial = a.create_state();
        let fini = a.create_state();
        a.set_accept(fini, true);
        a.add_transition(init, medial, 'm' as i32, 'm' as i32)?;
        a.add_transition(init, fini, 'm' as i32, 'm' as i32)?;
        a.add_transition(medial, fini, 'm' as i32, 'm' as i32)?;
        a.add_transition(fini, fini, 'm' as i32, 'm' as i32)?;
        a.finish_state()?;

        let prefix = Operations::get_common_prefix(&a)?;
        assert_eq!(prefix, "m");

        Ok(())
    }
    #[test]
    fn test_common_prefix_unicode() -> Result<()> {
        let a = Operations::concatenate(
            &Automata::make_string("boo😂😂😂")?,
            &Automata::make_any_char()?,
        )?;
        let prefix = Operations::get_common_prefix(&a)?;
        assert_eq!(prefix, "boo😂😂😂");
        Ok(())
    }

    #[test]
    fn test_concatenate1() -> Result<()> {
        let a =
            Operations::concatenate(&Automata::make_string("m")?, &Automata::make_any_string()?)?;
        assert!(Operations::run_str(&a, "m"));
        assert!(Operations::run_str(&a, "me"));
        assert!(Operations::run_str(&a, "me too"));
        Ok(())
    }

    #[test]
    fn test_concatenate2() -> Result<()> {
        let a = Operations::concatenate_with_list(&[
            &Automata::make_string("m")?,
            &Automata::make_any_string()?,
            &Automata::make_string("n")?,
            &Automata::make_any_string()?,
        ])?;
        let a = Operations::determinize(&a, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;

        assert!(Operations::run_str(&a, "mn"));
        assert!(Operations::run_str(&a, "mone"));
        assert!(!Operations::run_str(&a, "m"));
        assert!(!AutomatonTestUtil::is_finite(&a)?);

        Ok(())
    }
    #[test]
    fn test_union1() -> Result<()> {
        let a1 = Automata::make_string("foobar")?;
        let a2 = Automata::make_string("barbaz")?;

        let union = Operations::union_list(&[&a1, &a2])?;
        let det = Operations::determinize(&union, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;

        assert!(Operations::run_str(&det, "foobar"));
        assert!(Operations::run_str(&det, "barbaz"));

        assert_matches(&det, &["foobar", "barbaz"])?;
        Ok(())
    }
    #[test]
    fn test_union2() -> Result<()> {
        let a1 = Automata::make_string("foobar")?;
        let a2 = Automata::make_string("")?;
        let a3 = Automata::make_string("barbaz")?;

        let union = Operations::union_list(&[&a1, &a2, &a3])?;
        let det = Operations::determinize(&union, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;

        assert!(Operations::run_str(&det, "foobar"));
        assert!(Operations::run_str(&det, "barbaz"));
        assert!(Operations::run_str(&det, ""));

        assert_matches(&det, &["", "foobar", "barbaz"])?;
        Ok(())
    }
    #[test]
    fn test_minimize_simple() -> Result<()> {
        let a = Automata::make_string("foobar")?;
        let a_min =
            MinimizationOperations::minimize(&a, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;

        assert!(AutomatonTestUtil::same_language(&a, &a_min)?);
        Ok(())
    }
    #[test]
    fn test_minimize2() -> Result<()> {
        let a1 = Automata::make_string("foobar")?;
        let a2 = Automata::make_string("boobar")?;

        let union = Operations::union_list(&[&a1, &a2])?;
        let a_min =
            MinimizationOperations::minimize(&union, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;

        let removed = Operations::remove_dead_states(&union)?;
        let det = Operations::determinize(&removed, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;

        assert!(AutomatonTestUtil::same_language(&det, &a_min)?);
        Ok(())
    }

    #[test]
    fn test_reverse() -> Result<()> {
        let a = Automata::make_string("foobar")?;
        let ra = Operations::reverse(&a)?;
        let ra_rev = Operations::reverse(&ra)?;
        let a2 = Operations::determinize(&ra_rev, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;

        assert!(AutomatonTestUtil::same_language(&a, &a2)?);
        Ok(())
    }

    #[test]
    fn test_optional() -> Result<()> {
        let a = Automata::make_string("foobar")?;
        let a2 = Operations::optional(&a)?;
        let a2 = Operations::determinize(&a2, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;

        assert!(Operations::run_str(&a, "foobar"));
        assert!(!Operations::run_str(&a, ""));
        assert!(Operations::run_str(&a2, "foobar"));
        assert!(Operations::run_str(&a2, ""));
        Ok(())
    }

    #[test]
    fn test_repeat_any() -> Result<()> {
        let a = Automata::make_string("zee")?;
        let repeated = Operations::repeat(&a)?;
        let a2 = Operations::determinize(&repeated, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;

        assert!(Operations::run_str(&a2, ""));
        assert!(Operations::run_str(&a2, "zee"));
        assert!(Operations::run_str(&a2, "zeezee"));
        assert!(Operations::run_str(&a2, "zeezeezee"));
        Ok(())
    }
    #[test]
    fn test_repeat_min() -> Result<()> {
        let a = Automata::make_string("zee")?;
        let repeated = Operations::repeat_count(&a, 2)?;
        let a2 = Operations::determinize(&repeated, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;

        assert!(!Operations::run_str(&a2, ""));
        assert!(!Operations::run_str(&a2, "zee"));
        assert!(Operations::run_str(&a2, "zeezee"));
        assert!(Operations::run_str(&a2, "zeezeezee"));
        Ok(())
    }

    #[test]
    fn test_repeat_min_max1() -> Result<()> {
        let a = Automata::make_string("zee")?;
        let repeated = Operations::repeat_min_max(&a, 0, 2)?;
        let a2 = Operations::determinize(&repeated, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;

        assert!(Operations::run_str(&a2, ""));
        assert!(Operations::run_str(&a2, "zee"));
        assert!(Operations::run_str(&a2, "zeezee"));
        assert!(!Operations::run_str(&a2, "zeezeezee"));
        Ok(())
    }

    #[test]
    fn test_repeat_min_max2() -> Result<()> {
        let a = Automata::make_string("zee")?;
        let repeated = Operations::repeat_min_max(&a, 2, 4)?;
        let a2 = Operations::determinize(&repeated, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;

        assert!(!Operations::run_str(&a2, ""));
        assert!(!Operations::run_str(&a2, "zee"));
        assert!(Operations::run_str(&a2, "zeezee"));
        assert!(Operations::run_str(&a2, "zeezeezee"));
        assert!(Operations::run_str(&a2, "zeezeezeezee"));
        assert!(!Operations::run_str(&a2, "zeezeezeezeezee"));
        Ok(())
    }
    #[test]
    fn test_complement() -> Result<()> {
        let a = Automata::make_string("zee")?;
        let comp = Operations::complement(&a, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;
        let a2 = Operations::determinize(&comp, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;

        assert!(Operations::run_str(&a2, ""));
        assert!(!Operations::run_str(&a2, "zee"));
        assert!(Operations::run_str(&a2, "zeezee"));
        assert!(Operations::run_str(&a2, "zeezeezee"));
        Ok(())
    }

    #[test]
    fn test_interval() -> Result<()> {
        let interval = Automata::make_decimal_interval(17, 100, 3)?;
        let a = Operations::determinize(&interval, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;

        assert!(!Operations::run_str(&a, ""));
        assert!(Operations::run_str(&a, "017"));
        assert!(Operations::run_str(&a, "100"));
        assert!(Operations::run_str(&a, "073"));
        Ok(())
    }

    #[test]
    fn test_common_suffix() -> Result<()> {
        let mut a = Automaton::new();
        let init = a.create_state();
        let fini = a.create_state();
        a.set_accept(init, true);
        a.set_accept(fini, true);
        a.add_transition_label(init, fini, 'm' as i32)?;
        a.add_transition_label(fini, fini, 'm' as i32)?;
        a.finish_state()?;

        let suffix = Operations::get_common_suffix_bytes_ref(&a)?;
        assert_eq!(suffix.length, 0);
        Ok(())
    }
    #[test]
    fn test_common_suffix_empty() -> Result<()> {
        let a = Automata::make_empty()?;
        let suffix = Operations::get_common_suffix_bytes_ref(&a)?;
        assert_eq!(suffix, BytesRef::new());
        Ok(())
    }

    #[test]
    fn test_common_suffix_empty_string() -> Result<()> {
        let a = Automata::make_empty_string()?;
        let suffix = Operations::get_common_suffix_bytes_ref(&a)?;
        assert_eq!(suffix, BytesRef::new());
        Ok(())
    }

    #[test]
    fn test_common_suffix_trailing_wildcard() -> Result<()> {
        let a =
            Operations::concatenate(&Automata::make_string("boo")?, &Automata::make_any_char()?)?;
        let suffix = Operations::get_common_suffix_bytes_ref(&a)?;
        assert_eq!(suffix, BytesRef::new());
        Ok(())
    }

    #[test]
    fn test_common_suffix_leading_kleen_star() -> Result<()> {
        let mut random = random();
        let a = Operations::concatenate(
            &Automata::make_any_string()?,
            &Automata::make_string("boo")?,
        )?;
        let suffix = Operations::get_common_suffix_bytes_ref(&a)?;
        assert_eq!(suffix, new_bytes_ref_from_string(&mut random, "boo")?);
        Ok(())
    }

    #[test]
    fn test_common_suffix_trailing_kleen_star() -> Result<()> {
        let a = Operations::concatenate(
            &Automata::make_string("boo")?,
            &Automata::make_any_string()?,
        )?;
        let suffix = Operations::get_common_suffix_bytes_ref(&a)?;
        assert_eq!(suffix, BytesRef::new());
        Ok(())
    }

    #[test]
    fn test_common_suffix_unicode() -> Result<()> {
        let mut random = random();
        let a = Operations::concatenate(
            &Automata::make_any_string()?,
            &Automata::make_string("boo😂😂😂")?,
        )?;

        let binary = UTF32ToUTF8::default().convert(&a)?;
        let suffix = Operations::get_common_suffix_bytes_ref(&binary)?;

        assert_eq!(new_bytes_ref_from_string(&mut random, "boo😂😂😂")?, suffix);
        Ok(())
    }
    #[test]
    fn test_reverse_random1() -> Result<()> {
        let mut random = random();
        let iters = at_least(&mut random, 100);

        for _ in 0..iters {
            let a = AutomatonTestUtil::random_automaton(&mut random)?;
            let ra = Operations::reverse(&a)?;
            let rra = Operations::reverse(&ra)?;

            let v = Operations::remove_dead_states(&a)?;
            let orig = Operations::determinize(&v, i32::MAX as usize)?;
            let v = Operations::remove_dead_states(&rra)?;
            let reversed = Operations::determinize(&v, i32::MAX as usize)?;

            assert!(AutomatonTestUtil::same_language(&orig, &reversed)?);
        }

        Ok(())
    }
    #[test]
    fn test_reverse_random2() -> Result<()> {
        let mut random = random();
        let iters = at_least(&mut random, 100);

        for _ in 0..iters {
            let bool = random.random_bool(0.5);
            let seed: u64 = random.random();
            let mut a = AutomatonTestUtil::random_automaton(&mut random)?;
            if bool && let Cow::Owned(o) = Operations::remove_dead_states(&a)? {
                a = Cow::Owned(o)
            }

            let ra = Operations::reverse(&a)?;
            let rda = Operations::determinize(&ra, i32::MAX as usize)?;

            if Operations::is_empty(&a) {
                assert!(Operations::is_empty(&rda));
                continue;
            }

            let ras = RandomAcceptedStrings::new(&a)?;
            for _ in 0..20 {
                let mut random1 = random_from_seed(seed);
                // Find string accepted by original automaton
                let s = ras.get_random_accepted_string(&mut random1)?;
                let reversed: Vec<i32> = s.iter().copied().rev().collect();
                let len = reversed.len();
                let ints_ref = IntsRef::from_slice(reversed, 0, len);
                assert!(Operations::run_ints_ref(&rda, &ints_ref));
            }
        }

        Ok(())
    }
    #[test]
    fn test_any_string_empty_string() -> Result<()> {
        let any = Automata::make_any_string()?;
        let a = Operations::determinize(&any, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;
        assert!(Operations::run_str(&a, ""));
        Ok(())
    }

    #[test]
    fn test_basic_is_empty() -> Result<()> {
        let mut a = Automaton::new();
        a.create_state();
        assert!(Operations::is_empty(&a));
        Ok(())
    }

    #[test]
    fn test_remove_dead_transitions_empty() -> Result<()> {
        let a = Automata::make_empty()?;
        let a2 = Operations::remove_dead_states(&a)?;
        assert!(Operations::is_empty(&a2));
        Ok(())
    }
    #[test]
    #[should_panic(expected = "from state")]
    fn test_invalid_add_transition() {
        let mut a = Automaton::new();
        let s1 = a.create_state();
        let s2 = a.create_state();
        a.add_transition(s1, s2, 'a' as i32, 'a' as i32).unwrap();
        a.add_transition(s2, s2, 'a' as i32, 'a' as i32).unwrap();
        // This should panic because transitions on s1 were already added
        a.add_transition(s1, s2, 'b' as i32, 'b' as i32).unwrap();
    }
    #[test]
    fn test_builder_random() -> Result<()> {
        let mut random = random();
        let iters = at_least(&mut random, 100);

        for _ in 0..iters {
            let seed: u64 = random.random();
            let mut random1 = random_from_seed(seed);
            let a = AutomatonTestUtil::random_automaton(&mut random)?;

            let mut all_trans = vec![];
            let num_states = a.get_num_states();
            for s in 0..num_states {
                let count = a.get_num_transitions_with_state(s);
                for i in 0..count {
                    let mut t = Transition::default();
                    a.get_transition(s, i, &mut t);
                    all_trans.push(t);
                }
            }

            let mut builder = Builder::new();
            for i in 0..num_states {
                let s = builder.create_state();
                builder.set_accept(s, a.is_accept(i));
            }

            all_trans.shuffle(&mut random1);
            for t in all_trans {
                builder.add_transition(t.source, t.dest, t.min, t.max);
            }

            let v1 = Operations::remove_dead_states(&a)?;
            let a1 = Operations::determinize(&v1, usize::MAX)?;
            let b = builder.finish()?;
            let v2 = Operations::remove_dead_states(&b)?;
            let a2 = Operations::determinize(&v2, usize::MAX)?;
            assert!(AutomatonTestUtil::same_language(&a1, &a2)?);
        }

        Ok(())
    }
    #[test]
    fn test_is_total() -> Result<()> {
        assert!(!Operations::is_total(&Automaton::new())?);

        let mut a = Automaton::new();
        let init = a.create_state();
        let fini = a.create_state();
        a.set_accept(fini, true);
        a.add_transition(init, fini, char::MIN as i32, char::MAX as i32)?;
        a.finish_state()?;

        assert!(!Operations::is_total(&a)?);

        a.add_transition(fini, fini, char::MIN as i32, char::MAX as i32)?;
        a.finish_state()?;

        assert!(!Operations::is_total(&a)?);

        a.set_accept(init, true);
        let minimized =
            MinimizationOperations::minimize(&a, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;
        assert!(Operations::is_total(&minimized)?);

        Ok(())
    }

    #[test]
    fn test_minimize_empty() -> Result<()> {
        let mut a = Automaton::new();
        let init = a.create_state();
        let fini = a.create_state();
        a.add_transition_label(init, fini, 'a' as i32)?;
        a.finish_state()?;

        let a = MinimizationOperations::minimize(&a, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;
        assert_eq!(a.get_num_states(), 0);
        Ok(())
    }
    #[test]
    fn test_minus() -> Result<()> {
        let mut random = random();
        let a1 = Automata::make_string("foobar")?;
        let a2 = Automata::make_string("boobar")?;
        let a3 = Automata::make_string("beebar")?;

        let a = Operations::union_list(&[&a1, &a2, &a3])?;

        let a = if random.random_bool(0.5) {
            Operations::determinize(&a, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?
        } else if random.random_bool(0.5) {
            MinimizationOperations::minimize(&a, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?
        } else {
            Cow::Owned(a)
        };

        assert_matches(&a, &["foobar", "beebar", "boobar"])?;

        let a2 = Automata::make_string("boobar")?;
        let a4 = Operations::minus(&a, &a2, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;
        let a4 = Operations::determinize(&a4, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;

        assert!(Operations::run_str(&a4, "foobar"));
        assert!(!Operations::run_str(&a4, "boobar"));
        assert!(Operations::run_str(&a4, "beebar"));
        assert_matches(&a4, &["foobar", "beebar"])?;
        let a1 = Automata::make_string("foobar")?;
        let a4 = Operations::minus(&a4, &a1, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;
        let a4 = Operations::determinize(&a4, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;

        assert!(!Operations::run_str(&a4, "foobar"));
        assert!(!Operations::run_str(&a4, "boobar"));
        assert!(Operations::run_str(&a4, "beebar"));
        assert_matches(&a4, &["beebar"])?;
        let a3 = Automata::make_string("beebar")?;
        let a4 = Operations::minus(&a4, &a3, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;
        let a4 = Operations::determinize(&a4, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;

        assert!(!Operations::run_str(&a4, "foobar"));
        assert!(!Operations::run_str(&a4, "boobar"));
        assert!(!Operations::run_str(&a4, "beebar"));
        assert_matches(&a4, &[])?;

        Ok(())
    }
    #[test]
    fn test_one_interval() -> Result<()> {
        let a = Automata::make_decimal_interval(999, 1032, 0)?;
        let a = Operations::determinize(&a, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;

        assert!(Operations::run_str(&a, "0999"));
        assert!(Operations::run_str(&a, "00999"));
        assert!(Operations::run_str(&a, "000999"));
        Ok(())
    }

    #[test]
    fn test_another_interval() -> Result<()> {
        let a = Automata::make_decimal_interval(1, 2, 0)?;
        let a = Operations::determinize(&a, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;

        assert!(Operations::run_str(&a, "01"));
        Ok(())
    }
    #[test]
    fn test_interval_random() -> Result<()> {
        let mut random = random();
        let iters = at_least(&mut random, 100);

        for _ in 0..iters {
            let min = TestUtil::next_int(&mut random, 0, 100_000);
            let max = TestUtil::next_int(&mut random, min, min + 100_000);

            let digits = if random.random_bool(0.5) {
                0
            } else {
                let max_str = max.to_string();
                TestUtil::next_int(
                    &mut random,
                    max_str.len() as i32,
                    (2 * max_str.len()) as i32,
                )
            };

            let prefix = "0".repeat(digits as usize);

            let a = Automata::make_decimal_interval(min, max, digits)?;
            let a = Operations::determinize(&a, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;
            let a = if random.random_bool(0.5) {
                MinimizationOperations::minimize(&a, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?
            } else {
                a
            };

            let mut mins = min.to_string();
            let mut maxs = max.to_string();
            if digits > 0 {
                mins = format!("{}{}", &prefix[mins.len()..], mins);
                maxs = format!("{}{}", &prefix[maxs.len()..], maxs);
            }

            assert!(Operations::run_str(&a, &mins));
            assert!(Operations::run_str(&a, &maxs));

            for _ in 0..100 {
                let x = random.random_range(0..2 * max);
                let expected = x >= min && x <= max;
                let mut sx = x.to_string();

                if sx.len() < digits as usize {
                    sx = format!("{}{}", &prefix[sx.len()..], sx);
                } else if digits == 0 {
                    let num_zeros = random.random_range(0..10);
                    sx = format!("{}{}", "0".repeat(num_zeros), sx);
                }

                assert_eq!(Operations::run_str(&a, &sx), expected);
            }
        }

        Ok(())
    }

    fn assert_matches(a: &Automaton, strings: &[&str]) -> Result<()> {
        let mut expected = HashSet::new();
        let mut scratch = IntsRefBuilder::new();

        for s in strings {
            Util::get_utf32(s, &mut scratch);
            let v = scratch.get_owner();
            expected.insert(v);
        }
        let det = Operations::determinize(a, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;
        let actual =
            TestOperations::get_finite_strings(&det).expect("Failed to get finite strings");

        assert_eq!(expected, actual);
        Ok(())
    }
    #[test]
    fn test_concatenate_preserves_det() -> Result<()> {
        let a1 = Automata::make_string("foobar")?;
        assert!(a1.is_deterministic());

        let a2 = Automata::make_string("baz")?;
        assert!(a2.is_deterministic());

        let concat = Operations::concatenate_with_list(&[&a1, &a2])?;
        assert!(concat.is_deterministic());

        Ok(())
    }
    #[test]
    fn test_remove_dead_states() -> Result<()> {
        let a1 = Automata::make_string("x")?;
        let a2 = Automata::make_string("y")?;

        let a = Operations::concatenate_with_list(&[&a1, &a2])?;
        assert_eq!(a.get_num_states(), 4);

        let a = Operations::remove_dead_states(&a)?;
        assert_eq!(a.get_num_states(), 3);

        Ok(())
    }

    #[test]
    fn test_remove_dead_states_empty1() -> Result<()> {
        let mut a = Automaton::new();
        a.finish_state()?;
        assert!(Operations::is_empty(&a));

        let a2 = Operations::remove_dead_states(&a)?;
        assert!(Operations::is_empty(&a2));

        Ok(())
    }

    #[test]
    fn test_remove_dead_states_empty2() -> Result<()> {
        let mut a = Automaton::new();
        a.finish_state()?;
        assert!(Operations::is_empty(&a));

        let a2 = Operations::remove_dead_states(&a)?;
        assert!(Operations::is_empty(&a2));

        Ok(())
    }

    #[test]
    fn test_remove_dead_states_empty3() -> Result<()> {
        let mut a = Automaton::new();
        let init = a.create_state();
        let fini = a.create_state();
        a.add_transition_label(init, fini, 'a' as i32)?;

        let a2 = Operations::remove_dead_states(&a)?;
        assert_eq!(a2.get_num_states(), 0);

        Ok(())
    }
    #[test]
    fn test_concat_empty() -> Result<()> {
        let a = Operations::concatenate(&Automata::make_empty()?, &Automata::make_string("foo")?)?;
        let strings = TestOperations::get_finite_strings(&a)?;
        assert!(strings.is_empty());

        let a = Operations::concatenate(&Automata::make_string("foo")?, &Automata::make_empty()?)?;
        let strings = TestOperations::get_finite_strings(&a)?;
        assert!(strings.is_empty());

        Ok(())
    }

    #[test]
    fn test_seems_non_empty_but_is_not1() -> Result<()> {
        let mut a = Automaton::new();
        let init = a.create_state();
        let s = a.create_state();
        a.add_transition_label(init, s, 'a' as i32)?;
        a.finish_state()?;
        assert!(Operations::is_empty(&a));
        Ok(())
    }

    #[test]
    fn test_seems_non_empty_but_is_not2() -> Result<()> {
        let mut a = Automaton::new();
        let init = a.create_state();
        let s = a.create_state();
        a.add_transition_label(init, s, 'a' as i32)?;
        let orphan = a.create_state();
        a.set_accept(orphan, true);
        a.finish_state()?;
        assert!(Operations::is_empty(&a));
        Ok(())
    }
    #[test]
    fn test_same_language1() -> Result<()> {
        let a = Automata::make_empty_string()?;
        let mut a2 = Automata::make_empty_string()?;
        let state = a2.create_state();
        a2.add_transition_label(0, state, 'a' as i32)?;
        a2.finish_state()?;

        let a_removed = Operations::remove_dead_states(&a)?;
        let a2_removed = Operations::remove_dead_states(&a2)?;

        assert!(AutomatonTestUtil::same_language(&a_removed, &a2_removed)?);
        Ok(())
    }

    fn random_no_op<'a, R: Rng + ?Sized>(
        a: &'a Automaton,
        random: &mut R,
    ) -> Result<Cow<'a, Automaton>> {
        match random.random_range(0..7) {
            0 => Ok(Operations::determinize(a, i32::MAX as usize)?),
            1 => {
                if a.get_num_states() < 100 {
                    Ok(MinimizationOperations::minimize(
                        a,
                        Operations::DEFAULT_DETERMINIZE_WORK_LIMIT,
                    )?)
                } else {
                    Ok(Cow::Borrowed(a))
                }
            },
            2 => Ok(Operations::remove_dead_states(a)?),
            3 => {
                // reverse -> randomNoOp -> reverse
                let a0 = Operations::reverse(a)?;
                let a1 = random_no_op(&a0, random)?;
                Ok(Cow::Owned(Operations::reverse(&a1)?))
            },
            4 => Ok(Cow::Owned(Operations::concatenate(
                a,
                &Automata::make_empty_string()?,
            )?)),
            5 => {
                // union with empty automaton
                Ok(Cow::Owned(Operations::union(a, &Automata::make_empty()?)?))
            },
            6 => Ok(Cow::Borrowed(a)),
            _ => unreachable!(),
        }
    }
    fn has_massive_term(terms: &[BytesRef<Vec<u8>>]) -> bool {
        for term in terms {
            if term.length > Automata::MAX_STRING_UNION_TERM_LENGTH as usize {
                return true;
            }
        }
        false
    }
    fn union_terms<R: Rng + ?Sized>(terms: &[BytesRef<Vec<u8>>], rng: &mut R) -> Result<Automaton> {
        let a = if rng.random_bool(0.5) || has_massive_term(terms) {
            let owned_automata: Vec<Automaton> = terms
                .iter()
                .map(|term| Automata::make_string(&term.utf8_to_string()?))
                .collect::<Result<Vec<_>>>()?;
            let refs: Vec<&Automaton> = owned_automata.iter().collect();
            Operations::union_list(&refs)?
        } else {
            let mut terms_list = terms.to_vec();
            terms_list.sort();
            Automata::make_string_union(&terms_list)?
        };
        Ok(random_no_op(&a, rng)?.into_owned())
    }
    fn get_random_string<R: Rng + ?Sized>(random: &mut R) -> String {
        TestUtil::random_realistic_unicode_string(random)
    }
    #[test]
    fn test_random_finite() -> Result<()> {
        let mut random = random();
        let num_terms = at_least(&mut random, 10);
        let iters = at_least(&mut random, 100);

        let mut terms: BTreeSet<BytesRef<Vec<u8>>> = BTreeSet::new();
        while terms.len() < num_terms as usize {
            let s = get_random_string(&mut random);
            terms.insert(new_bytes_ref_from_string(&mut random, &s)?);
        }

        let mut a = Cow::Owned(union_terms(
            &terms.iter().cloned().collect::<Vec<_>>(),
            &mut random,
        )?);
        assert_same(&terms.iter().cloned().collect::<Vec<_>>(), &a, &mut random)?;

        for _ in 0..iters {
            match random.random_range(0..15) {
                0 => {
                    let string = get_random_string(&mut random);
                    let prefix = new_bytes_ref_from_string(&mut random, &string)?;
                    let mut new_terms = BTreeSet::new();
                    let mut new_term = BytesRefBuilder::new();
                    for term in &terms {
                        new_term.copy_bytes_with_ref(&prefix);
                        new_term.append_ref(term);
                        new_terms.insert(new_term.get_bytes_ref_copy());
                    }
                    terms = new_terms;
                    let was_deterministic1 = a.is_deterministic();
                    a = Cow::Owned(Operations::concatenate(
                        &Automata::make_string(&prefix.utf8_to_string()?)?,
                        &a,
                    )?);
                    assert_eq!(was_deterministic1, a.is_deterministic());
                },
                1 => {
                    let v = get_random_string(&mut random);
                    let suffix = new_bytes_ref_from_string(&mut random, &v)?;
                    let mut new_terms = BTreeSet::new();
                    let mut b = BytesRefBuilder::new();
                    for term in &terms {
                        b.copy_bytes_with_ref(term);
                        b.append_ref(&suffix);
                        new_terms.insert(b.get_bytes_ref_copy());
                    }
                    terms = new_terms;
                    a = Cow::Owned(Operations::concatenate(
                        &a,
                        &Automata::make_string(&suffix.utf8_to_string()?)?,
                    )?);
                },
                2 => {
                    if let Cow::Owned(a2) = Operations::determinize(&a, i32::MAX as usize)? {
                        a = Cow::Owned(a2);
                    }
                    assert!(a.is_deterministic());
                },
                3 => {
                    if a.get_num_states() < 100
                        && let Cow::Owned(a2) = MinimizationOperations::minimize(
                            &a,
                            Operations::DEFAULT_DETERMINIZE_WORK_LIMIT,
                        )?
                    {
                        a = Cow::Owned(a2);
                        assert!(a.is_deterministic());
                    }
                },
                4 => {
                    let mut new_terms = BTreeSet::new();
                    let num_new = random.random_range(0..5);
                    while new_terms.len() < num_new {
                        let s = get_random_string(&mut random);
                        new_terms.insert(new_bytes_ref_from_string(&mut random, &s)?);
                    }
                    let mut combined = terms.clone();
                    combined.extend(new_terms.iter().cloned());
                    let a2 =
                        union_terms(&new_terms.iter().cloned().collect::<Vec<_>>(), &mut random)?;
                    terms = combined;
                    a = Cow::Owned(Operations::union(&a, &a2)?);
                },
                5 => {
                    if let Cow::Owned(a2) = Operations::optional(&a)? {
                        a = Cow::Owned(a2);
                    }
                    terms.insert(new_bytes_ref_empty(&mut random)?);
                },
                6 => {
                    if !terms.is_empty() {
                        let v = Operations::remove_dead_states(&a)?;
                        let ras = RandomAcceptedStrings::new(&v)?;
                        let mut to_remove = BTreeSet::new();
                        let num_to_remove =
                            TestUtil::next_int(&mut random, 1, terms.len().div_ceil(2) as i32);
                        while to_remove.len() < num_to_remove as usize {
                            let ints = ras.get_random_accepted_string(&mut random)?;
                            let len = ints.len();
                            let s = new_bytes_ref_from_string(
                                &mut random,
                                &UnicodeUtil::new_string(&ints, 0, len)?,
                            )?;
                            if !to_remove.contains(&s) {
                                to_remove.insert(s);
                            }
                        }
                        for t in &to_remove {
                            let removed = terms.remove(t);
                            assert!(removed)
                        }
                        let a2 = union_terms(
                            &to_remove.iter().cloned().collect::<Vec<_>>(),
                            &mut random,
                        )?;
                        if let Cow::Owned(o) = Operations::minus(&a, &a2, i32::MAX as usize)? {
                            a = Cow::Owned(o);
                        }
                    }
                },
                7 => {
                    // minus infinite
                    let count = TestUtil::next_int(&mut random, 1, 5);
                    let mut prefixes = HashSet::new();
                    while prefixes.len() < count as usize {
                        let prefix = random.random_range(0..128);
                        prefixes.insert(prefix);
                    }

                    if cfg!(feature = "test_log_verbose") {
                        println!("  op=minus infinite prefixes={:?}", prefixes);
                    }

                    let mut as_ = vec![];

                    for &prefix in &prefixes {
                        let mut a2 = Automaton::new();
                        let init = a2.create_state();
                        let state = a2.create_state();
                        a2.add_transition_label(init, state, prefix)?;
                        a2.set_accept(state, true);
                        a2.add_transition(state, state, char::MIN as i32, char::MAX as i32)?;
                        a2.finish_state()?;
                        as_.push(a2);
                        terms.retain(|t| {
                            if t.length > 0 {
                                let first_byte = t.bytes[t.offset] as i32;
                                first_byte != prefix
                            } else {
                                true
                            }
                        });
                    }

                    let refs: Vec<&Automaton> = as_.iter().collect();
                    let v = Operations::union_list(&refs)?;
                    let a2 = random_no_op(&v, &mut random)?;
                    if let Cow::Owned(o) =
                        Operations::minus(&a, &a2, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?
                    {
                        a = Cow::Owned(o);
                    }
                },
                8 => {
                    let count = TestUtil::next_int(&mut random, 10, 20);
                    if cfg!(feature = "test_log_verbose") {
                        println!("  op=intersect infinite count={}", count);
                    }

                    let mut prefixes = HashSet::new();
                    while prefixes.len() < count as usize {
                        let prefix = random.random_range(0..128);
                        prefixes.insert(prefix);
                    }

                    if cfg!(feature = "test_log_verbose") {
                        println!("  prefixes={:?}", prefixes);
                    }

                    let mut as_ = vec![];

                    for &prefix in &prefixes {
                        let mut a2 = Automaton::new();
                        let init = a2.create_state();
                        let state = a2.create_state();
                        a2.add_transition_label(init, state, prefix)?;
                        a2.set_accept(state, true);
                        a2.add_transition(state, state, char::MIN as i32, char::MAX as i32)?;
                        a2.finish_state()?;
                        as_.push(a2);
                    }

                    let refs: Vec<&Automaton> = as_.iter().collect();
                    let mut a2 = Cow::Owned(Operations::union_list(&refs)?);
                    if random.random_bool(0.5) {
                        if let Cow::Owned(o) = Operations::determinize(
                            &a2,
                            Operations::DEFAULT_DETERMINIZE_WORK_LIMIT,
                        )? {
                            a2 = Cow::Owned(o);
                        }
                    } else if random.random_bool(0.5)
                        && let Cow::Owned(o) = MinimizationOperations::minimize(
                            &a2,
                            Operations::DEFAULT_DETERMINIZE_WORK_LIMIT,
                        )?
                    {
                        a2 = Cow::Owned(o);
                    }

                    if let Cow::Owned(o) = Operations::intersection(&a, &a2)? {
                        a = Cow::Owned(o);
                    }

                    terms.retain(|t| {
                        if t.length == 0 {
                            false
                        } else {
                            let first_byte = t.bytes[t.offset] as i32;
                            prefixes.contains(&first_byte)
                        }
                    });
                },

                9 => {
                    a = Cow::Owned(Operations::reverse(&a)?);
                    let mut reversed_terms = BTreeSet::new();
                    for t in &terms {
                        let rev = t.utf8_to_string()?.chars().rev().collect::<String>();
                        reversed_terms.insert(new_bytes_ref_from_string(&mut random, &rev)?);
                    }
                    terms = reversed_terms;
                },
                10 => {
                    if let Cow::Owned(o) = random_no_op(&a, &mut random)? {
                        a = Cow::Owned(o);
                    }
                },
                11 => {
                    let min = random.random_range(0..1000);
                    let max = min + random.random_range(0..50);
                    let digits = max.to_string().len();

                    if cfg!(feature = "test_log_verbose") {
                        println!(
                            "  op=union interval min={} max={} digits={}",
                            min, max, digits
                        );
                    }

                    let interval_automaton =
                        Automata::make_decimal_interval(min, max, digits as i32)?;
                    a = Cow::Owned(Operations::union(&a, &interval_automaton)?);

                    let prefix = "0".repeat(digits);
                    for i in min..=max {
                        let mut s = i.to_string();
                        if s.len() < digits {
                            s = format!("{}{}", &prefix[s.len()..], s);
                        }
                        terms.insert(new_bytes_ref_from_string(&mut random, &s)?);
                    }
                },
                12 => {
                    let v = Automata::make_empty_string()?;
                    if let Cow::Owned(o) =
                        Operations::minus(&a, &v, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?
                    {
                        a = Cow::Owned(o)
                    }
                    terms.remove(&new_bytes_ref_empty(&mut random)?);
                },
                13 => {
                    a = Cow::Owned(Operations::union(&a, &Automata::make_empty_string()?)?);
                    terms.insert(new_bytes_ref_empty(&mut random)?);
                },
                14 => {
                    if terms.len() <= (num_terms * 3) as usize {
                        if cfg!(feature = "test_log_verbose") {
                            println!("  op=concat finite automaton");
                        }

                        let count = if random.random_bool(0.5) { 2 } else { 3 };
                        let mut add_terms = BTreeSet::new();
                        while add_terms.len() < count {
                            let s = get_random_string(&mut random);
                            add_terms.insert(new_bytes_ref_from_string(&mut random, &s)?);
                        }

                        if cfg!(feature = "test_log_verbose") {
                            for term in &add_terms {
                                println!("    term={:?}", term);
                            }
                        }

                        let add_vec: Vec<_> = add_terms.iter().cloned().collect();
                        let a2 = union_terms(&add_vec, &mut random)?;

                        let mut new_terms = BTreeSet::new();

                        if random.random_bool(0.5) {
                            // suffix
                            if cfg!(feature = "test_log_verbose") {
                                println!("  do suffix");
                            }
                            let a2 = random_no_op(&a2, &mut random)?;
                            a = Cow::Owned(Operations::concatenate(&a, &a2)?);

                            let mut new_term = BytesRefBuilder::new();
                            for term in &terms {
                                for suffix in &add_terms {
                                    new_term.copy_bytes_with_ref(term);
                                    new_term.append_ref(suffix);
                                    new_terms.insert(new_term.get_bytes_ref_copy());
                                }
                            }
                        } else {
                            // prefix
                            if cfg!(feature = "test_log_verbose") {
                                println!("  do prefix");
                            }
                            let a2 = random_no_op(&a2, &mut random)?;
                            a = Cow::Owned(Operations::concatenate(&a2, &a)?);

                            let mut new_term = BytesRefBuilder::new();
                            for term in &terms {
                                for prefix in &add_terms {
                                    new_term.copy_bytes_with_ref(prefix);
                                    new_term.append_ref(term);
                                    new_terms.insert(new_term.get_bytes_ref_copy());
                                }
                            }
                        }

                        terms = new_terms;
                    }
                },

                _ => {}, // others omitted for brevity
            }
            assert_same(&terms.iter().cloned().collect::<Vec<_>>(), &a, &mut random)?;
            let left = AutomatonTestUtil::is_deterministic_slow(&a);
            let right = a.is_deterministic();
            assert_eq!(left, right);
            if random.random_range(0..10) == 7 {
                a = Cow::Owned(verify_topo_sort(&a)?)
            }
        }
        assert_same(&terms.iter().cloned().collect::<Vec<_>>(), &a, &mut random)?;

        Ok(())
    }
    /// Runs topo sort, verifies transitions then only "go forwards", and builds
    /// and returns new automaton with those remapped toposorted states.
    pub fn verify_topo_sort(a: &Automaton) -> Result<Automaton> {
        let sorted = Operations::topo_sort_states(a)?;
        // This can be < if we removed dead states:
        assert!(sorted.len() <= a.get_num_states() as usize);

        let mut a2 = Automaton::new();
        let mut state_map = vec![-1; a.get_num_states() as usize];
        let mut t = Transition::default();

        for &state in &sorted {
            let new_state = a2.create_state();
            let accept = a.is_accept(state);
            a2.set_accept(new_state, accept);
            assert_eq!(state_map[state as usize], -1);
            state_map[state as usize] = new_state;
        }
        // 2nd pass: add new transitions
        for &state in &sorted {
            let count = a.init_transition(state, &mut t);
            for _ in 0..count {
                a.get_next_transition(&mut t);
                assert!(state_map[t.dest as usize] > state_map[state as usize]);
                a2.add_transition(
                    state_map[state as usize],
                    state_map[t.dest as usize],
                    t.min,
                    t.max,
                )?;
            }
        }

        a2.finish_state()?;
        Ok(a2)
    }

    pub fn assert_same<R: Rng + ?Sized>(
        terms: &[BytesRef<Vec<u8>>],
        a: &Automaton,
        random: &mut R,
    ) -> Result<()> {
        assert!(AutomatonTestUtil::is_finite(a)?);
        assert!(!Operations::is_total(a)?);

        let det_a = Operations::determinize(a, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;

        // Make sure all terms are accepted:
        let mut scratch: IntsRefBuilder<Vec<i32>> = IntsRefBuilder::new();
        for term in terms {
            Util::get_ints_ref(term, &mut scratch);
            let s = term.utf8_to_string()?;
            assert!(
                Operations::run_str(&det_a, &s),
                "failed to accept term={}",
                s
            );
        }

        // Use getFiniteStrings:
        let mut expected = HashSet::new();
        for term in terms {
            let mut ints_ref = IntsRefBuilder::new();
            let s = term.utf8_to_string()?;
            Util::get_utf32(&s, &mut ints_ref);
            expected.insert(ints_ref.to_ints_ref());
        }
        let actual = TestOperations::get_finite_strings(a)?;

        if expected != actual {
            println!("FAILED: ");
            for term in &expected {
                if !actual.contains(term) {
                    println!("  term={:?} should be accepted but isn't", term);
                }
            }
            for term in &actual {
                if !expected.contains(term) {
                    println!("  term={:?} is accepted but should not be", term);
                }
            }
            unreachable!("mismatch");
        }
        // check same language via determinized unionTerms
        let v0 = &union_terms(terms, random)?;
        let v1 = Operations::determinize(v0, i32::MAX as usize)?;
        let a2 = Operations::remove_dead_states(&v1)?;
        let v0 = Operations::determinize(a, i32::MAX as usize)?;
        let a3 = Operations::remove_dead_states(&v0)?;
        assert!(AutomatonTestUtil::same_language(&a2, &a3)?);

        // check in UTF8 space
        let v = UTF32ToUTF8::default().convert(a)?;
        let utf8 = random_no_op(&v, random)?;

        let mut expected2 = HashSet::new();
        for term in terms {
            let mut ints_ref = IntsRefBuilder::new();
            Util::get_ints_ref(term, &mut ints_ref);
            expected2.insert(ints_ref.to_ints_ref());
        }

        assert_eq!(expected2, TestOperations::get_finite_strings(&utf8)?);

        Ok(())
    }
    fn accepts(a: &Automaton, b: &BytesRef<Vec<u8>>) -> Result<bool> {
        let mut builder = IntsRefBuilder::new();
        Util::get_ints_ref(b, &mut builder);
        Ok(Operations::run_ints_ref(a, builder.get()))
    }
    fn make_binary_interval(
        min_term: Option<BytesRef<Vec<u8>>>,
        min_inclusive: bool,
        max_term: Option<BytesRef<Vec<u8>>>,
        max_inclusive: bool,
    ) -> Result<Automaton> {
        let a = Automata::make_binary_interval(min_term, min_inclusive, max_term, max_inclusive)?;
        let min_a = MinimizationOperations::minimize(&a, i32::MAX as usize)?;

        if min_a.get_num_states() != a.get_num_states() {
            assert!(min_a.get_num_states() < a.get_num_states());
            return Err(LuceneError::illegal_state("automaton was not minimal"));
        }
        Ok(a)
    }
    #[test]
    fn test_make_binary_interval_finite_cases_basic() -> Result<()> {
        let zeros = vec![0u8; 3];
        let mut random = random();

        // 0 (incl) - 00 (incl)
        let a = make_binary_interval(
            Some(new_bytes_ref(&mut random, zeros.as_slice(), 0, 1)?),
            true,
            Some(new_bytes_ref(&mut random, zeros.as_slice(), 0, 2)?),
            true,
        )?;
        assert!(AutomatonTestUtil::is_finite(&a)?);
        assert!(!accepts(&a, &new_bytes_ref_empty(&mut random)?)?);
        assert!(accepts(
            &a,
            &new_bytes_ref(&mut random, zeros.as_slice(), 0, 1)?
        )?);
        assert!(accepts(
            &a,
            &new_bytes_ref(&mut random, zeros.as_slice(), 0, 2)?
        )?);
        assert!(!accepts(
            &a,
            &new_bytes_ref(&mut random, zeros.as_slice(), 0, 3)?
        )?);

        // '' (incl) - 00 (incl)
        let a = make_binary_interval(
            Some(new_bytes_ref_empty(&mut random)?),
            true,
            Some(new_bytes_ref(&mut random, zeros.as_slice(), 0, 2)?),
            true,
        )?;
        assert!(AutomatonTestUtil::is_finite(&a)?);
        assert!(accepts(&a, &new_bytes_ref_empty(&mut random)?)?);
        assert!(accepts(
            &a,
            &new_bytes_ref(&mut random, zeros.as_slice(), 0, 1)?
        )?);
        assert!(accepts(
            &a,
            &new_bytes_ref(&mut random, zeros.as_slice(), 0, 2)?
        )?);
        assert!(!accepts(
            &a,
            &new_bytes_ref(&mut random, zeros.as_slice(), 0, 3)?
        )?);

        // '' (excl) - 00 (incl)
        let a = make_binary_interval(
            Some(new_bytes_ref_empty(&mut random)?),
            false,
            Some(new_bytes_ref(&mut random, zeros.as_slice(), 0, 2)?),
            true,
        )?;
        assert!(AutomatonTestUtil::is_finite(&a)?);
        assert!(!accepts(&a, &new_bytes_ref_empty(&mut random)?)?);
        assert!(accepts(
            &a,
            &new_bytes_ref(&mut random, zeros.as_slice(), 0, 1)?
        )?);
        assert!(accepts(
            &a,
            &new_bytes_ref(&mut random, zeros.as_slice(), 0, 2)?
        )?);
        assert!(!accepts(
            &a,
            &new_bytes_ref(&mut random, zeros.as_slice(), 0, 3)?
        )?);

        // 0 (excl) - 00 (incl)
        let a = make_binary_interval(
            Some(new_bytes_ref(&mut random, zeros.as_slice(), 0, 1)?),
            false,
            Some(new_bytes_ref(&mut random, zeros.as_slice(), 0, 2)?),
            true,
        )?;
        assert!(AutomatonTestUtil::is_finite(&a)?);
        assert!(!accepts(&a, &new_bytes_ref_empty(&mut random)?)?);
        assert!(!accepts(
            &a,
            &new_bytes_ref(&mut random, zeros.as_slice(), 0, 1)?
        )?);
        assert!(accepts(
            &a,
            &new_bytes_ref(&mut random, zeros.as_slice(), 0, 2)?
        )?);
        assert!(!accepts(
            &a,
            &new_bytes_ref(&mut random, zeros.as_slice(), 0, 3)?
        )?);

        // 0 (excl) - 00 (excl)
        let a = make_binary_interval(
            Some(new_bytes_ref(&mut random, zeros.as_slice(), 0, 1)?),
            false,
            Some(new_bytes_ref(&mut random, zeros.as_slice(), 0, 2)?),
            false,
        )?;
        assert!(AutomatonTestUtil::is_finite(&a)?);
        assert!(!accepts(&a, &new_bytes_ref_empty(&mut random)?)?);
        assert!(!accepts(
            &a,
            &new_bytes_ref(&mut random, zeros.as_slice(), 0, 1)?
        )?);
        assert!(!accepts(
            &a,
            &new_bytes_ref(&mut random, zeros.as_slice(), 0, 2)?
        )?);
        assert!(!accepts(
            &a,
            &new_bytes_ref(&mut random, zeros.as_slice(), 0, 3)?
        )?);

        Ok(())
    }

    #[test]
    fn test_make_binary_interval_finite_cases_random() -> Result<()> {
        let mut random = random();
        let iters = at_least(&mut random, 100);

        for _ in 0..iters {
            let s = TestUtil::random_realistic_unicode_string(&mut random);
            let prefix = new_bytes_ref_from_string(&mut random, &s)?;

            let mut b = BytesRefBuilder::new();
            b.append_ref(&prefix);
            let num_zeros = random.random_range(0..10);
            for _ in 0..num_zeros {
                b.append_byte(0);
            }
            let min_term = b.get_bytes_ref_copy();

            let mut b = BytesRefBuilder::new();
            b.append_ref(&min_term);
            let num_zeros = random.random_range(0..10);
            for _ in 0..num_zeros {
                b.append_byte(0);
            }
            let max_term = b.get_bytes_ref_copy();

            let min_inclusive = random.random_bool(0.5);
            let max_inclusive = random.random_bool(0.5);

            let a = make_binary_interval(
                Some(min_term.clone()),
                min_inclusive,
                Some(max_term.clone()),
                max_inclusive,
            )?;
            assert!(AutomatonTestUtil::is_finite(&a)?);

            let mut expected_count = max_term.length as i32 - min_term.length as i32 + 1;
            if !min_inclusive {
                expected_count -= 1;
            }
            if !max_inclusive {
                expected_count -= 1;
            }

            if expected_count <= 0 {
                assert!(Operations::is_empty(&a));
                continue;
            } else {
                // Enumerate all finite strings and verify the count matches what we expect:
                let actual = TestOperations::get_finite_strings_with_limit(&a, expected_count)?;
                assert_eq!(expected_count as usize, actual.len());
            }

            let mut b = BytesRefBuilder::new();
            b.append_ref(&min_term);

            if !min_inclusive {
                assert!(!accepts(&a, &b.get_bytes_ref_copy())?);
                b.append_byte(0);
            }

            while b.length() < max_term.length {
                b.append_byte(0);

                let expected = if b.length() == max_term.length {
                    max_inclusive
                } else {
                    true
                };

                assert_eq!(expected, accepts(&a, &b.get_bytes_ref_copy())?);
            }
        }
        Ok(())
    }

    #[test]
    fn test_make_binary_interval_random() -> Result<()> {
        let mut random = random();
        let iters = at_least(&mut random, 100);

        for _ in 0..iters {
            let min_term = TestUtil::random_binary_term(&mut random);
            let min_inclusive = random.random_bool(0.5);
            let max_term = TestUtil::random_binary_term(&mut random);
            let max_inclusive = random.random_bool(0.5);

            let a = make_binary_interval(
                Some(min_term.clone()),
                min_inclusive,
                Some(max_term.clone()),
                max_inclusive,
            )?;

            for _ in 0..500 {
                let term = TestUtil::random_binary_term(&mut random);

                let min_cmp = min_term.cmp(&term).to_int();
                let max_cmp = max_term.cmp(&term).to_int();

                let expected = if min_cmp > 0 || max_cmp < 0 {
                    false
                } else if min_cmp == 0 && max_cmp == 0 {
                    min_inclusive && max_inclusive
                } else if min_cmp == 0 {
                    min_inclusive
                } else if max_cmp == 0 {
                    max_inclusive
                } else {
                    true
                };

                let mut ints_builder = IntsRefBuilder::new();
                Util::get_ints_ref(&term, &mut ints_builder);
                let actual = Operations::run_ints_ref(&a, &ints_builder.to_ints_ref());
                assert_eq!(expected, actual,);
            }
        }

        Ok(())
    }
    fn ints_ref<R: Rng + ?Sized>(s: &str, random: &mut R) -> Result<IntsRef<Vec<i32>>> {
        let mut builder = IntsRefBuilder::new();
        let b: BytesRef<Vec<u8>> = new_bytes_ref_from_string(random, s)?;
        Util::get_ints_ref(&b, &mut builder);
        Ok(builder.get().clone())
    }

    #[test]
    fn test_make_binary_interval_basic() -> Result<()> {
        let mut random = random();

        let a = Automata::make_binary_interval(
            Some(new_bytes_ref_from_string(&mut random, "bar")?),
            true,
            Some(new_bytes_ref_from_string(&mut random, "foo")?),
            true,
        )?;
        assert!(Operations::run_ints_ref(&a, &ints_ref("bar", &mut random)?));
        assert!(Operations::run_ints_ref(&a, &ints_ref("foo", &mut random)?));
        assert!(Operations::run_ints_ref(
            &a,
            &ints_ref("beep", &mut random)?
        ));
        assert!(!Operations::run_ints_ref(
            &a,
            &ints_ref("baq", &mut random)?
        ));
        assert!(Operations::run_ints_ref(
            &a,
            &ints_ref("bara", &mut random)?
        ));

        Ok(())
    }

    #[test]
    fn test_make_binary_interval_lower_bound_empty_string() -> Result<()> {
        let mut random = random();

        let a = Automata::make_binary_interval(
            Some(new_bytes_ref_from_string(&mut random, "")?),
            true,
            Some(new_bytes_ref_from_string(&mut random, "bar")?),
            true,
        )?;
        assert!(Operations::run_ints_ref(&a, &ints_ref("", &mut random)?));
        assert!(Operations::run_ints_ref(&a, &ints_ref("a", &mut random)?));
        assert!(Operations::run_ints_ref(&a, &ints_ref("bar", &mut random)?));
        assert!(!Operations::run_ints_ref(
            &a,
            &ints_ref("bara", &mut random)?
        ));
        assert!(!Operations::run_ints_ref(
            &a,
            &ints_ref("baz", &mut random)?
        ));

        let a = Automata::make_binary_interval(
            Some(new_bytes_ref_from_string(&mut random, "")?),
            false,
            Some(new_bytes_ref_from_string(&mut random, "bar")?),
            true,
        )?;
        assert!(!Operations::run_ints_ref(&a, &ints_ref("", &mut random)?));
        assert!(Operations::run_ints_ref(&a, &ints_ref("a", &mut random)?));
        assert!(Operations::run_ints_ref(&a, &ints_ref("bar", &mut random)?));
        assert!(!Operations::run_ints_ref(
            &a,
            &ints_ref("bara", &mut random)?
        ));
        assert!(!Operations::run_ints_ref(
            &a,
            &ints_ref("baz", &mut random)?
        ));

        Ok(())
    }
    #[test]
    fn test_make_binary_interval_equal() -> Result<()> {
        let mut random = random();

        let a = Automata::make_binary_interval(
            Some(new_bytes_ref_from_string(&mut random, "bar")?),
            true,
            Some(new_bytes_ref_from_string(&mut random, "bar")?),
            true,
        )?;
        assert!(Operations::run_ints_ref(&a, &ints_ref("bar", &mut random)?));
        assert!(AutomatonTestUtil::is_finite(&a)?);
        let strings = TestOperations::get_finite_strings(&a)?;
        assert_eq!(1, strings.len());

        Ok(())
    }
    #[test]
    fn test_make_binary_interval_common_prefix() -> Result<()> {
        let mut random = random();

        let a = Automata::make_binary_interval(
            Some(new_bytes_ref_from_string(&mut random, "bar")?),
            true,
            Some(new_bytes_ref_from_string(&mut random, "barfoo")?),
            true,
        )?;
        assert!(!Operations::run_ints_ref(
            &a,
            &ints_ref("bam", &mut random)?
        ));
        assert!(Operations::run_ints_ref(&a, &ints_ref("bar", &mut random)?));
        assert!(Operations::run_ints_ref(
            &a,
            &ints_ref("bara", &mut random)?
        ));
        assert!(Operations::run_ints_ref(
            &a,
            &ints_ref("barf", &mut random)?
        ));
        assert!(Operations::run_ints_ref(
            &a,
            &ints_ref("barfo", &mut random)?
        ));
        assert!(Operations::run_ints_ref(
            &a,
            &ints_ref("barfoo", &mut random)?
        ));
        assert!(Operations::run_ints_ref(
            &a,
            &ints_ref("barfonz", &mut random)?
        ));
        assert!(!Operations::run_ints_ref(
            &a,
            &ints_ref("barfop", &mut random)?
        ));
        assert!(!Operations::run_ints_ref(
            &a,
            &ints_ref("barfoop", &mut random)?
        ));

        Ok(())
    }
    #[test]
    fn test_make_binary_except_empty() -> Result<()> {
        let mut random = random();

        let a = Automata::make_non_empty_binary()?;
        assert!(!Operations::run_ints_ref(&a, &ints_ref("", &mut random)?));

        let s = TestUtil::random_realistic_unicode_string_range(&mut random, 1, 10);
        assert!(Operations::run_ints_ref(&a, &ints_ref(&s, &mut random)?));

        Ok(())
    }
    #[test]
    fn test_make_binary_interval_open_max() -> Result<()> {
        let mut random = random();

        let a = Automata::make_binary_interval(
            Some(new_bytes_ref_from_string(&mut random, "bar")?),
            true,
            None,
            true,
        )?;

        assert!(!Operations::run_ints_ref(
            &a,
            &ints_ref("bam", &mut random)?
        ));
        assert!(Operations::run_ints_ref(&a, &ints_ref("bar", &mut random)?));
        assert!(Operations::run_ints_ref(
            &a,
            &ints_ref("bara", &mut random)?
        ));
        assert!(Operations::run_ints_ref(
            &a,
            &ints_ref("barf", &mut random)?
        ));
        assert!(Operations::run_ints_ref(
            &a,
            &ints_ref("barfo", &mut random)?
        ));
        assert!(Operations::run_ints_ref(
            &a,
            &ints_ref("barfoo", &mut random)?
        ));
        assert!(Operations::run_ints_ref(
            &a,
            &ints_ref("barfonz", &mut random)?
        ));
        assert!(Operations::run_ints_ref(
            &a,
            &ints_ref("barfop", &mut random)?
        ));
        assert!(Operations::run_ints_ref(
            &a,
            &ints_ref("barfoop", &mut random)?
        ));
        assert!(Operations::run_ints_ref(&a, &ints_ref("zzz", &mut random)?));

        Ok(())
    }
    #[test]
    fn test_make_binary_interval_open_max_zero_length_min() -> Result<()> {
        let mut random = random();

        let a = Automata::make_binary_interval(
            Some(new_bytes_ref_from_string(&mut random, "")?),
            true,
            None,
            true,
        )?;

        assert!(Operations::run_ints_ref(&a, &ints_ref("", &mut random)?));
        assert!(Operations::run_ints_ref(&a, &ints_ref("a", &mut random)?));
        assert!(Operations::run_ints_ref(
            &a,
            &ints_ref("aaaaaa", &mut random)?
        ));

        let a = Automata::make_binary_interval(
            Some(new_bytes_ref_from_string(&mut random, "")?),
            false,
            None,
            true,
        )?;

        assert!(!Operations::run_ints_ref(&a, &ints_ref("", &mut random)?));
        assert!(Operations::run_ints_ref(&a, &ints_ref("a", &mut random)?));
        assert!(Operations::run_ints_ref(
            &a,
            &ints_ref("aaaaaa", &mut random)?
        ));

        Ok(())
    }
    #[test]
    fn test_make_binary_interval_open_min() -> Result<()> {
        let mut random = random();

        let a = Automata::make_binary_interval(
            None,
            true,
            Some(new_bytes_ref_from_string(&mut random, "foo")?),
            true,
        )?;

        assert!(!Operations::run_ints_ref(
            &a,
            &ints_ref("foz", &mut random)?
        ));
        assert!(!Operations::run_ints_ref(
            &a,
            &ints_ref("zzz", &mut random)?
        ));
        assert!(Operations::run_ints_ref(&a, &ints_ref("foo", &mut random)?));
        assert!(Operations::run_ints_ref(&a, &ints_ref("", &mut random)?));
        assert!(Operations::run_ints_ref(&a, &ints_ref("a", &mut random)?));
        assert!(Operations::run_ints_ref(&a, &ints_ref("aaa", &mut random)?));
        assert!(Operations::run_ints_ref(&a, &ints_ref("bz", &mut random)?));

        Ok(())
    }
    #[test]
    fn test_make_binary_interval_open_both() -> Result<()> {
        let mut random = random();

        let a = Automata::make_binary_interval(None, true, None, true)?;

        assert!(Operations::run_ints_ref(&a, &ints_ref("foz", &mut random)?));
        assert!(Operations::run_ints_ref(&a, &ints_ref("zzz", &mut random)?));
        assert!(Operations::run_ints_ref(&a, &ints_ref("foo", &mut random)?));
        assert!(Operations::run_ints_ref(&a, &ints_ref("", &mut random)?));
        assert!(Operations::run_ints_ref(&a, &ints_ref("a", &mut random)?));
        assert!(Operations::run_ints_ref(&a, &ints_ref("aaa", &mut random)?));
        assert!(Operations::run_ints_ref(&a, &ints_ref("bz", &mut random)?));

        Ok(())
    }
    #[test]
    fn test_accept_all_empty_string_min() -> Result<()> {
        let mut random = random();

        let a = Automata::make_binary_interval(
            Some(new_bytes_ref_from_string(&mut random, "")?),
            true,
            None,
            true,
        )?;
        let any = Automata::make_any_binary()?;
        assert!(AutomatonTestUtil::same_language(&any, &a)?);

        Ok(())
    }
    fn to_ints_ref(s: &str) -> IntsRef<Vec<i32>> {
        let mut builder = IntsRefBuilder::new();
        for ch in s.chars() {
            builder.append(ch as i32);
        }
        builder.get().clone()
    }
    #[test]
    fn test_get_singleton() -> Result<()> {
        let mut random = random();
        let iters = at_least(&mut random, 10_000);

        for _ in 0..iters {
            let s = TestUtil::random_realistic_unicode_string(&mut random);
            let a = Automata::make_string(&s)?;
            assert_eq!(to_ints_ref(&s), Operations::get_singleton(&a)?.unwrap());
        }

        Ok(())
    }
    #[test]
    fn test_get_singleton_empty_string() -> Result<()> {
        let mut a = Automaton::new();
        let s = a.create_state();
        a.set_accept(s, true);
        a.finish_state()?;
        assert_eq!(IntsRef::new(), Operations::get_singleton(&a)?.unwrap());
        Ok(())
    }

    #[test]
    fn test_get_singleton_nothing() -> Result<()> {
        let mut a = Automaton::new();
        a.create_state();
        a.finish_state()?;
        assert!(Operations::get_singleton(&a)?.is_none());
        Ok(())
    }

    #[test]
    fn test_get_singleton_two() -> Result<()> {
        let mut a = Automaton::new();
        let s = a.create_state();
        let x = a.create_state();
        a.set_accept(x, true);
        a.add_transition_label(s, x, 55)?;
        let y = a.create_state();
        a.set_accept(y, true);
        a.add_transition_label(s, y, 58)?;
        a.finish_state()?;
        assert!(Operations::get_singleton(&a)?.is_none());
        Ok(())
    }
    // LUCENE-9981
    #[test]
    fn test_determinize_too_much_effort() {
        // make sure determinize properly aborts, relatively quickly, for this regexp:
        let result = (|| {
            let a = RegExp::from_string("(.*a){2000}")?.to_automaton()?;
            Operations::determinize(&a, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;
            Ok::<(), LuceneError>(())
        })();
        assert!(matches!(
            result,
            Err(LuceneError::TooComplexToDeterminize(_))
        ));

        let result = (|| {
            let a = RegExp::from_string("(.*a){2000}")?.to_automaton()?;
            let rev = Operations::reverse(&a)?;
            Operations::determinize(&rev, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;
            Ok::<(), LuceneError>(())
        })();
        assert!(matches!(
            result,
            Err(LuceneError::TooComplexToDeterminize(_))
        ));
    }
}
