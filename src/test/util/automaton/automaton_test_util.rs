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
use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use bit_set::BitSet;
use rand::Rng;
use rand::prelude::IndexedRandom;

use crate::core::util::automation::automaton::{Automaton, Builder};
use crate::core::util::automation::operations::Operations;
use crate::core::util::automation::reg_exp::RegExp;
use crate::core::util::automation::state_pair::StatePair;
use crate::core::util::automation::transition::Transition;
use crate::core::util::automation::transition_accessor::TransitionAccessor;
use crate::core::util::error::lucene_error::LuceneError;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::ints_ref::IntsRef;
use crate::core::util::ints_ref_builder::IntsRefBuilder;
use crate::core::util::unicode_util::UnicodeUtil;
/// Utilities for testing automata.
///
/// Capable of generating random regular expressions and automata,
/// and also provides a number of very basic, unoptimized (*slow)
/// implementations for testing.
pub struct AutomatonTestUtil;
impl AutomatonTestUtil {
    /// Default maximum number of states that {@link Operations#determinize}
    /// should create.
    pub const DEFAULT_MAX_DETERMINIZED_STATES: usize = 1000000;
    ///  Maximum level of recursion allowed in recursive operations.
    pub const MAX_RECURSION_LEVEL: usize = 1000;
    pub(crate) fn random_regexp<R: Rng + ?Sized>(random: &mut R) -> Result<String> {
        loop {
            let regexp = Self::random_regexp_string(random);
            if !UnicodeUtil::valid_utf16_string(regexp.as_str()) {
                continue;
            }
            let result = RegExp::parse(&regexp, RegExp::NONE, 0);
            if result.is_ok() {
                return Ok(regexp);
            }
        }
    }
    fn random_regexp_string<R: Rng + ?Sized>(random: &mut R) -> String {
        let end = random.random_range(0..20);
        let mut result = String::with_capacity(end * 2);
        let specials = ['.', '?', '*', '+', '(', ')', '-', '[', ']', '|'];

        let mut i = 0;
        while i < end {
            let t = random.random_range(0..15);
            if t == 0 && i < end - 1 {
                let codepoint = random.random_range(0x10000..=0x10FFFF);
                if let Some(ch) = std::char::from_u32(codepoint) {
                    result.push(ch);
                    i += 1;
                }
            } else if t <= 1 {
                result.push(char::from_u32(random.random_range(0x00..0x80)).unwrap());
            } else if t == 2 {
                result.push(char::from_u32(random.random_range(0x80..0x800)).unwrap());
            } else if t == 3 {
                result.push(char::from_u32(random.random_range(0x800..0xD800)).unwrap());
            } else if t == 4 {
                result.push(char::from_u32(random.random_range(0xE000..=0xFFFF)).unwrap());
            } else {
                result.push(specials[t - 5]);
            }
            i += 1;
        }
        result
    }
    /// picks a random int code point, avoiding surrogates; throws
    /// IllegalArgumentException if this transition only accepts surrogates
    fn get_random_codepoint<R: Rng + ?Sized>(random: &mut R, min: i32, max: i32) -> Result<i32> {
        let code = if max < UnicodeUtil::UNI_SUR_HIGH_START || min > UnicodeUtil::UNI_SUR_LOW_END {
            // Entire range is outside surrogates
            random.random_range(min..=max)
        } else if min >= UnicodeUtil::UNI_SUR_HIGH_START {
            if max > UnicodeUtil::UNI_SUR_LOW_END {
                // Range is after surrogates
                random.random_range(UnicodeUtil::UNI_SUR_LOW_END + 1..=max)
            } else {
                return Err(LuceneError::illegal_argument(format!(
                    "transition accepts only surrogates: min={} max={}",
                    min, max
                )));
            }
        } else if max <= UnicodeUtil::UNI_SUR_LOW_END {
            if min < UnicodeUtil::UNI_SUR_HIGH_START {
                // Range is before surrogates
                random.random_range(min..UnicodeUtil::UNI_SUR_HIGH_START)
            } else {
                return Err(LuceneError::illegal_argument(format!(
                    "transition accepts only surrogates: min={} max={}",
                    min, max
                )));
            }
        } else {
            // Range spans surrogates; we skip the surrogate block
            let gap1 = UnicodeUtil::UNI_SUR_HIGH_START - min;
            let gap2 = max - UnicodeUtil::UNI_SUR_LOW_END;
            let c = random.random_range(0..gap1 + gap2);
            if c < gap1 {
                min + c
            } else {
                UnicodeUtil::UNI_SUR_LOW_END + (c - gap1) + 1
            }
        };

        assert!(
            code >= min
                && code <= max
                && !(UnicodeUtil::UNI_SUR_HIGH_START..=UnicodeUtil::UNI_SUR_LOW_END)
                    .contains(&code),
            "code={} min={} max={}",
            code,
            min,
            max
        );
        Ok(code)
    }

    pub fn random_single_automaton<R: Rng + ?Sized>(random: &mut R) -> Result<Automaton> {
        loop {
            let pattern = AutomatonTestUtil::random_regexp(random)?;
            match RegExp::from_str_with_flags(&pattern, RegExp::NONE)
                .and_then(|r| r.to_automaton())
                .and_then(|a| {
                    if random.random_bool(0.5) {
                        Operations::complement(&a, Self::DEFAULT_MAX_DETERMINIZED_STATES)
                    } else {
                        Ok(a)
                    }
                }) {
                Ok(a) => return Ok(a),
                Err(LuceneError::TooComplexToDeterminize(_)) => continue,
                Err(e) => return Err(e),
            }
        }
    }

    /// return a random NFA/DFA for testing
    pub fn random_automaton<R: Rng + ?Sized>(random: &mut R) -> Result<Cow<'_, Automaton>> {
        let a1 = AutomatonTestUtil::random_single_automaton(random)?;
        let a2 = AutomatonTestUtil::random_single_automaton(random)?;

        match random.random_range(0..4) {
            0 => Ok(Cow::Owned(Operations::concatenate(&a1, &a2)?)),
            1 => Ok(Cow::Owned(Operations::union(&a1, &a2)?)),
            2 => Ok(Cow::Owned(Operations::intersection(&a1, &a2)?.into_owned())),
            _ => Ok(Cow::Owned(
                Operations::minus(&a1, &a2, Self::DEFAULT_MAX_DETERMINIZED_STATES)?.into_owned(),
            )),
        }
    }
    /**
     * below are original, unoptimized implementations of DFA operations for
     * testing. These are from brics automaton, full license (BSD)
     * below:
     */
    /*
     * dk.brics.automaton
     *
     * Copyright (c) 2001-2009 Anders Moeller
     * All rights reserved.
     *
     * Redistribution and use in source and binary forms, with or without
     * modification, are permitted provided that the following conditions
     * are met:
     * 1. Redistributions of source code must retain the above copyright notice, this list of
     *    conditions and the following disclaimer.
     * 2. Redistributions in binary form must reproduce the above copyright notice, this list of
     *    conditions and the following disclaimer in the documentation and/or other materials
     *    provided with the distribution.
     * 3. The name of the author may not be used to endorse or promote products derived from this
     *    software without specific prior written permission.
     *
     * THIS SOFTWARE IS PROVIDED BY THE AUTHOR ``AS IS'' AND ANY EXPRESS OR
     * IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES
     * OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED.
     * IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR ANY DIRECT, INDIRECT,
     * INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT
     * NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
     * DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
     * THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
     * (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF
     * THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
     */
    /// Simple, original brics implementation of Brzozowski minimize()
    pub fn minimize_simple(a: &Automaton) -> Result<Cow<'_, Automaton>> {
        let mut initial_set = BTreeSet::new();
        let v = Operations::reverse_with_initial_states(a, Option::from(&mut initial_set))?;
        let a = Self::determinize_simple_with_set(&v, Rc::new(initial_set.clone()))?;

        initial_set.clear();
        let v = Operations::reverse_with_initial_states(&a, Option::from(&mut initial_set))?;
        match Self::determinize_simple_with_set(&v, Rc::new(initial_set))? {
            Cow::Borrowed(_) => Ok(Cow::Owned(v)),
            Cow::Owned(o) => Ok(Cow::Owned(o)),
        }
    }
    /// Simple, original brics implementation of determinize()
    pub fn determinize_simple(a: &Automaton) -> Result<Cow<'_, Automaton>> {
        let mut initial_set = BTreeSet::new();
        initial_set.insert(0);
        Self::determinize_simple_with_set(a, Rc::new(initial_set))
    }
    /// Simple, original brics implementation of determinize() Determinizes the
    /// given automaton using  the given set of initial states.
    pub fn determinize_simple_with_set(
        a: &Automaton,
        initial_set: Rc<BTreeSet<i32>>,
    ) -> Result<Cow<'_, Automaton>> {
        if a.get_num_states() == 0 {
            return Ok(Cow::Borrowed(a));
        }

        let points = a.get_start_points();
        let mut sets: HashMap<Rc<BTreeSet<i32>>, Rc<BTreeSet<i32>>> = HashMap::new();
        let mut worklist: VecDeque<Rc<BTreeSet<i32>>> = VecDeque::new();
        let mut newstate: HashMap<Rc<BTreeSet<i32>>, i32> = HashMap::new();

        sets.insert(initial_set.clone(), initial_set.clone());
        worklist.push_back(initial_set.clone());

        let mut result = Builder::default();
        result.create_state();
        newstate.insert(initial_set.clone(), 0);

        let mut t = Transition::default();

        while let Some(s) = worklist.pop_front() {
            let r = *newstate.get(&s).unwrap();
            if s.iter().any(|&q| a.is_accept(q)) {
                result.set_accept(r, true);
            }
            for n in 0..points.len() {
                let mut p = BTreeSet::new();

                for &q in s.iter() {
                    let count = a.init_transition(q, &mut t);
                    for _ in 0..count {
                        a.get_next_transition(&mut t);
                        if t.min <= points[n] && points[n] <= t.max {
                            p.insert(t.dest);
                        }
                    }
                }
                let p = Rc::new(p);

                if !sets.contains_key(&p) {
                    sets.insert(p.clone(), p.clone());
                    worklist.push_back(p.clone());
                    let new_state = result.create_state();
                    newstate.insert(p.clone(), new_state);
                }
                let q = *newstate.get(&p).unwrap();
                let min = points[n];
                let max = if n + 1 < points.len() {
                    points[n + 1] - 1
                } else {
                    char::MAX as i32
                };

                result.add_transition(r, q, min, max);
            }
        }

        let automaton = result.finish()?;
        match Operations::remove_dead_states(&automaton)? {
            Cow::Borrowed(_) => Ok(Cow::Owned(automaton)),
            Cow::Owned(o) => Ok(Cow::Owned(o)),
        }
    }

    /// Simple, original implementation of `get_finite_strings`.
    ///
    /// Returns the set of accepted strings, assuming that at most `limit`
    /// strings are accepted. If more than `limit` strings are accepted, the
    /// first `limit` strings found are returned. If `limit < 0`, then the
    /// limit is considered infinite.
    ///
    /// This implementation is recursive: it uses one stack frame for each
    /// character in the returned strings (i.e., the maximum is the maximum
    /// length of the returned strings).
    pub fn get_finite_strings_recursive(a: &Automaton, limit: i32) -> HashSet<IntsRef<Vec<i32>>> {
        let mut strings = HashSet::new();
        let mut path_states = HashSet::new();
        let mut path = IntsRefBuilder::new();

        if !Self::get_finite_strings(a, 0, &mut path_states, &mut strings, &mut path, limit) {
            return strings;
        }
        strings
    }
    /// Returns the strings that can be produced from the given state,
    /// or `false` if more than `limit` strings are found.
    ///
    /// A `limit` less than `0` means "infinite".
    fn get_finite_strings(
        a: &Automaton,
        s: i32,
        path_states: &mut HashSet<i32>,
        strings: &mut HashSet<IntsRef<Vec<i32>>>,
        path: &mut IntsRefBuilder<Vec<i32>>,
        limit: i32,
    ) -> bool {
        path_states.insert(s);

        let mut t = Transition::default();
        let count = a.init_transition(s, &mut t);

        for _ in 0..count {
            a.get_next_transition(&mut t);
            if path_states.contains(&t.dest) {
                return false;
            }
            for label in t.min..=t.max {
                path.append(label);

                if a.is_accept(t.dest) {
                    strings.insert(path.to_ints_ref());
                    if limit >= 0 && strings.len() > limit as usize {
                        return false;
                    }
                }

                if !Self::get_finite_strings(a, t.dest, path_states, strings, path, limit) {
                    return false;
                }
                path.set_length(path.length() - 1);
            }
        }

        path_states.remove(&s);
        true
    }
    /// Returns `true` if the language of this automaton is finite.
    /// The automaton must not have any dead states.
    pub(crate) fn is_finite(a: &Automaton) -> Result<bool> {
        if a.get_num_states() == 0 {
            return Ok(true);
        }
        let mut scratch = Transition::default();
        let mut path = BitSet::with_capacity(a.get_num_states() as usize);
        let mut visited = BitSet::with_capacity(a.get_num_states() as usize);
        Self::is_finite_inner(&mut scratch, a, 0, &mut path, &mut visited, 0)
    }

    /// Checks whether there is a loop containing the given state.
    /// (This is sufficient since there are never transitions to dead states.)
    pub(crate) fn is_finite_inner(
        scratch: &mut Transition,
        a: &Automaton,
        state: i32,
        path: &mut BitSet,
        visited: &mut BitSet,
        level: usize,
    ) -> Result<bool> {
        if level > Self::MAX_RECURSION_LEVEL {
            return Err(LuceneError::illegal_argument(format!(
                "input automaton is too large: level={}",
                level
            )));
        }

        path.insert(state as usize);
        let num_transitions = a.init_transition(state, scratch);

        for t in 0..num_transitions {
            a.get_transition(state, t, scratch);
            let dest = scratch.dest;
            if path.contains(dest as usize)
                || (!visited.contains(dest as usize)
                    && !Self::is_finite_inner(scratch, a, dest, path, visited, level + 1)?)
            {
                return Ok(false);
            }
        }

        path.remove(state as usize);
        visited.insert(state as usize);
        Ok(true)
    }
    /// Returns true if the automaton is deterministic.
    pub fn is_deterministic_slow(a: &Automaton) -> bool {
        let mut t = Transition::default();
        let num_states = a.get_num_states();
        for s in 0..num_states {
            let count = a.init_transition(s, &mut t);
            let mut last_max = -1;
            for _ in 0..count {
                a.get_next_transition(&mut t);
                if t.min <= last_max {
                    assert!(!a.is_deterministic());
                    return false;
                }
                last_max = t.max;
            }
        }

        assert!(a.is_deterministic());
        true
    }

    /// Returns `true` if these two automata accept exactly the same language.
    /// This is a costly computation!
    ///
    /// Both automata must be determinized and have no dead states.
    pub(crate) fn same_language(a1: &Automaton, a2: &Automaton) -> Result<bool> {
        if std::ptr::eq(a1, a2) {
            return Ok(true);
        }
        Ok(AutomatonTestUtil::subset_of(a2, a1)? && AutomatonTestUtil::subset_of(a1, a2)?)
    }

    /// Returns `true` if the language of `a1` is a subset of the language of
    /// `a2`. Both automata must be determinized and must have no dead
    /// states.
    ///
    /// Complexity: quadratic in the number of states.
    pub(crate) fn subset_of(a1: &Automaton, a2: &Automaton) -> Result<bool> {
        if !a1.is_deterministic() {
            return Err(LuceneError::illegal_argument("a1 must be deterministic"));
        }
        if !a2.is_deterministic() {
            return Err(LuceneError::illegal_argument("a2 must be deterministic"));
        }
        assert!(!Operations::has_dead_states_from_initial(a1)?);
        assert!(!Operations::has_dead_states_from_initial(a2)?);

        if a1.get_num_states() == 0 {
            return Ok(true);
        } else if a2.get_num_states() == 0 {
            return Ok(Operations::is_empty(a1));
        }

        let transitions1 = a1.get_sorted_transitions();
        let transitions2 = a2.get_sorted_transitions();

        let mut worklist = VecDeque::new();
        let mut visited = HashSet::new();

        let p = Rc::new(StatePair::new(0, 0));
        worklist.push_back(p.clone());
        visited.insert(p);

        while let Some(p) = worklist.pop_front() {
            if a1.is_accept(p.s1) && !a2.is_accept(p.s2) {
                return Ok(false);
            }

            let t1 = &transitions1[p.s1 as usize];
            let t2 = &transitions2[p.s2 as usize];

            let mut b2 = 0;
            for t1n in t1.iter() {
                while b2 < t2.len() && t2[b2].max < t1n.min {
                    b2 += 1;
                }

                let mut min1 = t1n.min;
                let mut max1 = t1n.max;

                for t2n in &t2[b2..] {
                    if t1n.max < t2n.min {
                        break;
                    }
                    if t2n.min > min1 {
                        return Ok(false);
                    }

                    if t2n.max < char::MAX as i32 {
                        min1 = t2n.max + 1;
                    } else {
                        min1 = char::MAX as i32;
                        max1 = char::MIN as i32;
                    }

                    let q = Rc::new(StatePair::new(t1n.dest, t2n.dest));
                    if visited.insert(q.clone()) {
                        worklist.push_back(q);
                    }
                }

                if min1 <= max1 {
                    return Ok(false);
                }
            }
        }

        Ok(true)
    }
}
#[derive(Eq, PartialEq, Clone)]
struct HashSetAsKey {
    set: Rc<BTreeSet<i32>>,
}
impl Hash for HashSetAsKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        for i in self.set.iter() {
            i.hash(state);
        }
    }
}
/// Allows retrieving random strings accepted by an [`Automaton`].
///
/// Once created, call [`RandomAcceptedStrings::get_random_accepted_string`] to
/// get a new string (in UTF-32 codepoints).
pub struct RandomAcceptedStrings<'a> {
    leads_to_accept: HashMap<Transition, bool>,
    a: &'a Automaton,
    transitions: Vec<Vec<Transition>>,
}
impl<'a> RandomAcceptedStrings<'a> {
    pub fn new(a: &'a Automaton) -> Result<Self> {
        if a.get_num_states() == 0 {
            return Err(LuceneError::illegal_argument(
                "this automaton accepts nothing",
            ));
        }

        let transitions = a.get_sorted_transitions();

        let mut leads_to_accept = HashMap::new();
        let mut all_arriving: HashMap<i32, Vec<ArrivingTransition>> = HashMap::new();

        let mut q = VecDeque::new();
        let mut seen = HashSet::new();
        // reverse map the transitions, so we can quickly look
        // up all arriving transitions to a given state
        let num_states = a.get_num_states();
        for s in 0..num_states {
            for t in &transitions[s as usize] {
                let tl = all_arriving.get_mut(&t.dest);
                match tl {
                    Some(v) => v.push(ArrivingTransition {
                        from: s,
                        t: t.clone(),
                    }),
                    None => {
                        let tl_new = vec![ArrivingTransition {
                            from: s,
                            t: t.clone(),
                        }];

                        all_arriving.insert(t.dest, tl_new);
                    },
                }
            }

            if a.is_accept(s) {
                q.push_back(s);
                seen.insert(s);
            }
        }

        // Breadth-first search, from accept states,
        // backwards:
        while let Some(s) = q.pop_front() {
            if let Some(arriving) = all_arriving.get(&s) {
                for at in arriving {
                    let from = at.from;
                    if !seen.contains(&from) {
                        q.push_back(from);
                        seen.insert(from);
                        leads_to_accept.insert(at.t.clone(), true);
                    }
                }
            }
        }

        Ok(Self {
            leads_to_accept,
            a,
            transitions,
        })
    }
    pub(crate) fn get_random_accepted_string<R: Rng + ?Sized>(
        &self,
        random: &mut R,
    ) -> Result<Vec<i32>> {
        let mut codepoints = Vec::new();
        let mut s = 0;

        loop {
            if self.a.is_accept(s)
                && (self.a.get_num_transitions_with_state(s) == 0 || random.random_bool(0.5))
            {
                break;
            }

            let transitions = &self.transitions[s as usize];
            if transitions.is_empty() {
                return Err(LuceneError::illegal_state("this automaton has dead states"));
            }

            let cheat = random.random_bool(0.5);
            let t = if cheat {
                let to_accept: Vec<&Transition> = transitions
                    .iter()
                    .filter(|t| self.leads_to_accept.contains_key(*t))
                    .collect();

                if to_accept.is_empty() {
                    transitions.choose(random).unwrap()
                } else {
                    *to_accept.choose(random).unwrap()
                }
            } else {
                transitions.choose(random).unwrap()
            };

            let codepoint = AutomatonTestUtil::get_random_codepoint(random, t.min, t.max)?;
            codepoints.push(codepoint);
            s = t.dest;
        }

        Ok(codepoints)
    }
}

struct ArrivingTransition {
    from: i32,
    t: Transition,
}
