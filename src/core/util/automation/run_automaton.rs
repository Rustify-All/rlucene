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
use std::fmt;
use std::hash::{Hash, Hasher};

use crate::core::util::accountable::Accountable;
use crate::core::util::automation::automaton::Automaton;
use crate::core::util::automation::transition::Transition;
use crate::core::util::bit_set::BitSet;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::LuceneError;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::fixed_bit_set::FixedBitSet;
/// Finite-state automaton with fast run operation. The initial state is always
/// 0.
pub struct RunAutomaton {
    pub(crate) automaton: Automaton,
    alphabet_size: usize,
    size: i32,
    accept: FixedBitSet,
    transitions: Vec<i32>, // transitions[state * points.len() + get_char_class(c)]
    points: Vec<i32>,
    classmap: Vec<usize>,
}

impl RunAutomaton {
    /// Constructs a new [`RunAutomaton`] from a deterministic [`Automaton`].
    ///
    /// Parameters:
    /// - `a`: an automaton
    ///
    /// Errors:
    /// - Returns an error if the automaton is not deterministic.
    pub fn new(automaton: Automaton, alphabet_size: usize) -> Result<Self> {
        if !automaton.is_deterministic() {
            return Err(LuceneError::illegal_argument(
                "Automaton must be deterministic",
            ));
        }

        let points = automaton.get_start_points();
        let size = std::cmp::max(1, automaton.get_num_states());
        let mut accept = FixedBitSet::new(size as usize);
        let mut transitions = vec![-1; size as usize * points.len()];

        let mut transition = Transition::default();
        for n in 0..size {
            if automaton.is_accept(n) {
                accept.set(n as usize);
            }
            transition.source = n;
            transition.transition_upto = -1;

            for (c_idx, &point) in points.iter().enumerate() {
                let dest = automaton.next(&mut transition, point);
                debug_assert!(dest == -1 || dest < size);
                transitions[n as usize * points.len() + c_idx] = dest;
            }
        }

        let mut classmap = vec![0; alphabet_size.min(256)];
        let mut i = 0;
        for (j, class) in classmap.iter_mut().enumerate() {
            if i + 1 < points.len() && j as i32 == points[i + 1] {
                i += 1;
            }
            *class = i;
        }

        Ok(Self {
            automaton,
            alphabet_size,
            size,
            accept,
            transitions,
            points,
            classmap,
        })
    }
    /// Returns number of states in automaton.
    pub fn size(&self) -> i32 {
        self.size
    }

    /// Returns the acceptance status for the given state.
    ///
    /// Parameters:
    /// - `state`: The state to check
    ///
    /// Returns:
    /// - `true` if the state is an accept state, otherwise `false`.
    pub fn is_accept(&self, state: i32) -> Result<bool> {
        self.accept.get(state as usize)
    }

    /// Returns array of codepoint class interval start points. The array should
    /// not be modified by the caller.
    pub fn char_intervals(&self) -> &[i32] {
        self.points.as_slice()
    }
    /// Gets character class of given codepoint
    fn get_char_class(&self, c: i32) -> usize {
        let mut a = 0;
        let mut b = self.points.len();
        while b - a > 1 {
            let d = (a + b) / 2;
            if self.points[d] > c {
                b = d;
            } else if self.points[d] < c {
                a = d;
            } else {
                return d;
            }
        }
        a
    }
    /// Returns the state obtained by reading the given character from the given
    /// state. Returns `-1` if no such state can be obtained.
    ///
    /// (If the original [`Automaton`] had no dead states, then `-1` is returned
    /// here if and only if a dead state would be entered in an equivalent
    /// automaton with a total transition function.)
    pub fn step(&self, state: i32, c: i32) -> i32 {
        debug_assert!((c as usize) < self.alphabet_size);
        let class = if c as usize >= self.classmap.len() {
            self.get_char_class(c)
        } else {
            self.classmap[c as usize]
        };
        self.transitions[state as usize * self.points.len() + class]
    }
}
impl fmt::Display for RunAutomaton {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "initial state: 0")?;
        for i in 0..self.size {
            write!(f, "state {i}")?;
            match self.accept.get(i as usize) {
                Ok(true) => write!(f, " [accept]:")?,
                Ok(false) => write!(f, " [reject]:")?,
                Err(_) => return Err(fmt::Error),
            }

            for j in 0..self.points.len() {
                let k = self.transitions[i as usize * self.points.len() + j];
                if k != -1 {
                    let min = self.points[j];
                    let max = if j + 1 < self.points.len() {
                        self.points[j + 1] - 1
                    } else {
                        self.alphabet_size as i32
                    };

                    write!(f, " ")?;
                    Automaton::append_char_string(min, f)?;
                    if min != max {
                        write!(f, "-")?;
                        Automaton::append_char_string(max, f)?;
                    }
                    writeln!(f, " -> {k}")?;
                }
            }
        }
        Ok(())
    }
}

impl Accountable for RunAutomaton {
    fn ram_bytes_used(&self) -> Result<i64> {
        // TODO:memory calculation not Implement
        Ok(0)
    }
}
impl Hash for RunAutomaton {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.alphabet_size.hash(state);
        self.size.hash(state);
        self.points.hash(state);
    }
}
use std::cmp::PartialEq;

impl PartialEq for RunAutomaton {
    fn eq(&self, other: &Self) -> bool {
        self.alphabet_size == other.alphabet_size
            && self.size == other.size
            && self.points == other.points
            && self.accept == other.accept
            && self.transitions == other.transitions
    }
}

impl Eq for RunAutomaton {}
