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
use crate::core::index::filtered_terms_enum::{
    AcceptStatus, FilteredTermsEnum, FilteredTermsEnumBase,
};
use crate::core::index::terms_enum::TermsEnum;
use crate::core::index::{BytesRef, BytesRefBuilder};
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::automation::byte_runnable::{ByteRunnable, ByteRunnableEnum};
use crate::core::util::automation::compiled_automaton::{AutomatonType, CompiledAutomaton};
use crate::core::util::automation::transition::Transition;
use crate::core::util::automation::transition_accessor::{
    TransitionAccessor, TransitionAccessorEnum,
};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::ints_ref_builder::IntsRefBuilder;
use crate::core::util::{SliceCopyOps, StringHelper, ToInt};
use std::rc::Rc;

/// A [`FilteredTermsEnum`](crate::core::index::filtered_terms_enum::FilteredTermsEnum) that enumerates terms based on what is accepted by a
/// DFA.
///
/// The algorithm works as follows:
///
/// 1. As long as matches are successful, keep reading sequentially.
/// 2. When a match fails, skip to the next string in lexicographic order that
///    does **not** enter a reject state.
///
/// Note:
/// - The algorithm does **not** attempt to skip directly to the next completely
///   accepted string, which is not possible when the language accepted by the
///   DFA is infinite (e.g., due to `*` operator).
pub struct AutomatonTermsEnum {
    /// A tableized array-based form of the DFA.
    byte_runnable: ByteRunnableEnum,
    /// Common suffix of the automaton.
    common_suffix_ref: Option<Rc<BytesRef<Vec<u8>>>>,
    // true if the automaton accepts a finite language
    finite: bool,
    // array of sorted transitions for each state, indexed by state number
    transition_accessor: TransitionAccessorEnum,
    // Used for visited state tracking: each short records gen when we last
    // visited the state; we use gens to avoid having to clear
    visited: Vec<u16>,
    cur_gen: u16,
    // the reference used for seeking forwards through the term dictionary
    seek_bytes_ref: BytesRefBuilder<Vec<u8>>,
    // true if we are enumerating an infinite portion of the DFA.
    // in this case it is faster to drive the query based on the terms dictionary.
    // when this is true, linearUpperBound indicate the end of range
    // of terms where we should simply do sequential reads instead.
    linear: bool,
    linear_upper_bound: BytesRef<Vec<u8>>,
    transition: Transition,
    saved_states: IntsRefBuilder<Vec<i32>>,
    start_term: Option<BytesRef<Vec<u8>>>,
}
impl AutomatonTermsEnum {
    pub fn new<TE>(
        tenum: TE,
        compiled: &mut CompiledAutomaton,
    ) -> Result<FilteredTermsEnum<TE, Self>>
    where
        TE: TermsEnum,
    {
        Self::with_start_term(tenum, compiled, None)
    }
    pub fn with_start_term<TE>(
        tenum: TE,
        compiled: &mut CompiledAutomaton,
        start_term: Option<&BytesRef<Vec<u8>>>,
    ) -> Result<FilteredTermsEnum<TE, Self>>
    where
        TE: TermsEnum,
    {
        if compiled.automaton_type != AutomatonType::Normal {
            return Err(LuceneError::illegal_argument(
                "please use CompiledAutomaton.get_terms_enum instead",
            ));
        }

        let byte_runnable = compiled.get_byte_runnable();
        let visited = if compiled.finite {
            Vec::new()
        } else {
            vec![0u16; byte_runnable.get_size() as usize]
        };
        let sub = Self {
            // FilteredTermsEnum parent initialization — you'd handle this separately
            byte_runnable,
            transition_accessor: compiled.get_transition_accessor(),
            common_suffix_ref: compiled.common_suffix_ref.clone(),
            finite: compiled.finite,
            visited,
            cur_gen: 0,
            seek_bytes_ref: BytesRefBuilder::new(),
            linear: false,
            linear_upper_bound: BytesRef::new(),
            transition: Transition::default(),
            saved_states: IntsRefBuilder::new(),
            start_term: start_term.cloned(),
        };
        Ok(FilteredTermsEnum::new(tenum, sub))
    }

    /// Records the given state has been visited.
    fn set_visited(&mut self, state: i32) {
        debug_assert!(state >= 0);
        let state = state as usize;
        if !self.finite {
            if state >= self.visited.len() {
                ArrayUtil::grow_with_len(&mut self.visited, state + 1);
            }
            self.visited[state] = self.cur_gen;
        }
    }
    /// Indicates whether the given state has been visited.
    fn is_visited(&self, state: i32) -> bool {
        debug_assert!(state >= 0);
        let state = state as usize;
        !self.finite && state < self.visited.len() && self.visited[state] == self.cur_gen
    }

    /// Sets the enum to operate in linear fashion after detecting a looping
    /// transition at a given position.
    ///
    /// This sets an upper bound and behaves like a `TermRangeQuery` for this
    /// portion of the term space.
    fn set_linear(&mut self, position: usize) {
        debug_assert!(!self.linear);

        let mut state = 0;
        let mut max_interval = 0xFF;

        for i in 0..position {
            let byte = self.seek_bytes_ref.byte_at(i);
            state = self.byte_runnable.step(state, byte as i32);
            debug_assert!(state >= 0, "state = {state}");
        }

        let num_transitions = self
            .transition_accessor
            .get_num_transitions_with_state(state);
        self.transition_accessor
            .init_transition(state, &mut self.transition);

        for _ in 0..num_transitions {
            self.transition_accessor
                .get_next_transition(&mut self.transition);
            let ch = self.seek_bytes_ref.byte_at(position) as i32;
            if self.transition.min <= ch && ch <= self.transition.max {
                max_interval = self.transition.max as u8;
                break;
            }
        }
        // 0xff terms don't get the optimization... not worth the trouble.
        max_interval = max_interval.saturating_add(1);

        let length = position + 1;

        if self.linear_upper_bound.bytes.len() < length {
            let new_len = ArrayUtil::oversize(length, 1);
            ArrayUtil::grow_with_len(&mut self.linear_upper_bound.bytes, new_len);
        }

        self.linear_upper_bound.bytes[..position]
            .copy_from(&self.seek_bytes_ref.bytes_ref().bytes[0..position], 0);
        self.linear_upper_bound.bytes[position] = max_interval;
        self.linear_upper_bound.length = length;
        self.linear = true;
    }
    /// Increments the byte buffer to the next string in binary order after `s`
    /// that will not put the machine into a reject state. If no such string
    /// exists, returns `false`.
    ///
    /// The correctness of this method depends on:
    /// - The automaton being deterministic
    /// - No transitions leading to dead states
    ///
    /// Returns:
    /// - `true` if more possible solutions exist for the DFA; otherwise,
    ///   `false`.
    pub fn next_string(&mut self) -> Result<bool> {
        let mut state;
        let mut pos: usize = 0;

        self.saved_states.grow(self.seek_bytes_ref.length() + 1);
        self.saved_states.set_int_at(0, 0);

        loop {
            if !self.finite {
                self.cur_gen = self.cur_gen.wrapping_add(1);
                if self.cur_gen == 0 {
                    // Clear the visited states every time curGen wraps (so very infrequently to not
                    // impact average perf).
                    for v in &mut self.visited {
                        *v = u16::MAX;
                    }
                }
            }

            self.linear = false;

            // walk the automaton until a character is rejected
            state = self.saved_states.int_at(pos);
            while pos < self.seek_bytes_ref.length() {
                self.set_visited(state);
                let byte = self.seek_bytes_ref.byte_at(pos) as i32;
                let next_state = self.byte_runnable.step(state, byte);
                if next_state == -1 {
                    break;
                }
                self.saved_states.set_int_at(pos + 1, next_state);
                if !self.linear && self.is_visited(next_state) {
                    self.set_linear(pos);
                }
                state = next_state;
                pos += 1;
            }

            // take the useful portion, and the last non-reject state, and attempt to
            // append characters that will match.
            if self.next_string_with_position(state, pos)? {
                return Ok(true);
            } else {
                /* no more solutions exist from this useful portion, backtrack  */
                let v = self.backtrack(pos);
                if v < 0 {
                    /* no more solutions at all  */
                    return Ok(false);
                }
                pos = v as usize;

                let prev_state = self.saved_states.int_at(pos);
                let byte = self.seek_bytes_ref.byte_at(pos) as i32;
                let new_state = self.byte_runnable.step(prev_state, byte);

                if new_state >= 0 && self.byte_runnable.is_accept(new_state)? {
                    /* String is good to go as-is  */
                    return Ok(true);
                }

                if !self.finite {
                    pos = 0;
                }
            }
        }
    }

    /// Returns the next string in lexicographic order that will not put the
    /// machine into a reject state.
    ///
    /// This method traverses the DFA from the given position in the string,
    /// starting at the given state.
    ///
    /// If the machine cannot be satisfied from this position, returns `false`.
    /// The traversal follows the minimal path in lexicographic order as far
    /// as possible.
    ///
    /// Note:
    /// - If this method returns `false`, there might still be more solutions;
    ///   it is necessary to backtrack to continue the search.
    ///
    /// Parameters:
    /// - `state`: The current non-reject state.
    /// - `position`: The portion of the string to use.
    ///
    /// Returns:
    /// - `true` if more possible solutions exist for the DFA from this
    ///   position.
    fn next_string_with_position(&mut self, mut state: i32, position: usize) -> Result<bool> {
        // The next lexicographic character must be greater than the existing character.
        let mut c = 0;
        if position < self.seek_bytes_ref.length() {
            c = self.seek_bytes_ref.byte_at(position);
            // if the next byte is 0xff and is not part of the useful portion,
            // then by definition it puts us in a reject state, and therefore this
            // path is dead. there cannot be any higher transitions. backtrack.
            if c == 0xFF {
                return Ok(false);
            }
            c += 1;
        }

        self.seek_bytes_ref.set_length(position);
        self.set_visited(state);

        let num_transitions = self
            .transition_accessor
            .get_num_transitions_with_state(state);
        self.transition_accessor
            .init_transition(state, &mut self.transition);

        // find the minimal path (lexicographic order) that is >= c
        let c = c as i32;
        for _ in 0..num_transitions {
            self.transition_accessor
                .get_next_transition(&mut self.transition);
            if self.transition.max >= c {
                let next_char = self.transition.min.max(c);
                // append either the next sequential char, or the minimum transition
                self.seek_bytes_ref.append_byte(next_char as u8);
                state = self.transition.dest;
                // as long as is possible, continue down the minimal path in
                // lexicographic order. if a loop or accept state is encountered, stop.
                // descend minimal lex path until loop or accept state
                while !self.is_visited(state) && !self.byte_runnable.is_accept(state)? {
                    self.set_visited(state);
                    // Note: we work with a DFA with no transitions to dead states.
                    // so the below is ok, if it is not an accept state,
                    // then there MUST be at least one transition.
                    self.transition_accessor
                        .init_transition(state, &mut self.transition);
                    self.transition_accessor
                        .get_next_transition(&mut self.transition);
                    state = self.transition.dest;
                    // append the minimum transition
                    self.seek_bytes_ref.append_byte(self.transition.min as u8);
                    // we found a loop, record it for faster enumeration
                    if !self.linear && self.is_visited(state) {
                        self.set_linear(self.seek_bytes_ref.length() - 1);
                    }
                }
                return Ok(true);
            }
        }

        Ok(false)
    }

    /// Attempts to backtrack through the string after encountering a dead end
    /// at the given position. Returns `false` if no more possible strings
    /// can match.
    ///
    /// Parameters:
    /// - `position`: The current position in the input string.
    ///
    /// Returns:
    /// - A position `>= 0` if more possible solutions exist for the DFA;
    ///   otherwise, returns `false`.
    fn backtrack(&mut self, mut position: usize) -> i32 {
        debug_assert!(position < i32::MAX as usize);
        while position > 0 {
            position -= 1;
            let next_char = self.seek_bytes_ref.byte_at(position);
            // if a character is 0xff it's a dead-end too,
            // because there is no higher character in binary sort order.
            if next_char != 0xFF {
                self.seek_bytes_ref.set_byte_at(position, next_char + 1);
                self.seek_bytes_ref.set_length(position + 1);
                return position as i32;
            }
        }
        -1 // all solutions exhausted
    }
}
impl FilteredTermsEnumBase for AutomatonTermsEnum {
    fn accept(&mut self, term: &BytesRef<Vec<u8>>, _ord: i64) -> Result<AcceptStatus> {
        let suffix_ok = match &self.common_suffix_ref {
            None => true,
            Some(suffix) => StringHelper::ends_with(term, suffix),
        };

        let v = if suffix_ok {
            if self
                .byte_runnable
                .run(&term.bytes, term.offset, term.length)?
            {
                if self.linear {
                    AcceptStatus::Yes
                } else {
                    AcceptStatus::YesAndSeek
                }
            } else if self.linear && term.cmp(&self.linear_upper_bound).to_int() < 0 {
                AcceptStatus::No
            } else {
                AcceptStatus::NoAndSeek
            }
        } else if self.linear && term.cmp(&self.linear_upper_bound).to_int() < 0 {
            AcceptStatus::No
        } else {
            AcceptStatus::NoAndSeek
        };
        Ok(v)
    }

    fn next_seek_term(
        &mut self,
        term: Option<&BytesRef<Vec<u8>>>,
    ) -> Result<Option<BytesRef<Vec<u8>>>> {
        if let Some(t) = term {
            self.seek_bytes_ref.copy_bytes_with_ref(t);
        } else {
            match self.start_term {
                Some(ref t) => self.seek_bytes_ref.copy_bytes_with_ref(t),
                None => {
                    debug_assert_eq!(self.seek_bytes_ref.length(), 0);
                    // return the empty term, as it's valid
                    if self.byte_runnable.is_accept(0)? {
                        return Ok(Some(self.seek_bytes_ref.get_bytes_owner()));
                    }
                },
            }
        }

        let v = if self.next_string()? {
            Some(self.seek_bytes_ref.get_bytes_owner())
        } else {
            None
        };
        Ok(v)
    }
}
