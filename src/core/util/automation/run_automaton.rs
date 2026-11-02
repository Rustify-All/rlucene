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
use crate::core::util::ram_usage_estimator::size_of_slice;
/// Finite-state automaton with fast run operation. The initial state is always
/// 0.
#[derive(Clone)]
pub struct RunAutomaton {
  alphabet_size: usize,
  size: usize,
  pub(crate) tables: Arc<RunAutomatonTables>,
}

pub(crate) struct RunAutomatonTables {
  pub(crate) automaton: Automaton,
  accept: FixedBitSet,
  transitions: Box<[i32]>, // transitions[state * points.len() + get_char_class(c)]
  points: Box<[i32]>,
  classmap: Box<[u16]>,
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
    let automaton_states = automaton.get_num_states();
    let state_count = std::cmp::max(1, automaton_states);
    let size = state_count as usize;
    let mut accept = FixedBitSet::new(size);
    let mut transitions = vec![-1; size * points.len()];

    let mut transition = Transition::default();
    // The empty language retains one rejecting run state, but has no source
    // state to read from the automaton's initialized-state vector.
    for n in 0..automaton_states {
      let state_index = n as usize;
      if automaton.is_accept(n) {
        accept.set(state_index)?;
      }
      transition.source = n;
      transition.transition_upto = None;

      for (c_idx, &point) in points.iter().enumerate() {
        let dest = automaton.next(&mut transition, point);
        debug_assert!(dest == -1 || dest < state_count);
        transitions[state_index * points.len() + c_idx] = dest;
      }
    }

    let mut classmap = vec![0; alphabet_size.min(256)];
    let mut i = 0;
    for (j, class) in classmap.iter_mut().enumerate() {
      if i + 1 < points.len() && j as i32 == points[i + 1] {
        i += 1;
      }
      // There are at most 256 iterations, each increasing i at most once.
      *class = i as u16;
    }

    Ok(Self {
      alphabet_size,
      size,
      tables: Arc::new(RunAutomatonTables {
        automaton,
        accept,
        transitions: transitions.into_boxed_slice(),
        points: points.into_boxed_slice(),
        classmap: classmap.into_boxed_slice(),
      }),
    })
  }
  /// Returns number of states in automaton.
  pub fn size(&self) -> usize {
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
    self.tables.accept.get(state as usize)
  }

  /// Returns array of codepoint class interval start points. The array should
  /// not be modified by the caller.
  pub fn char_intervals(&self) -> &[i32] {
    self.tables.points.as_ref()
  }
  /// Gets character class of given codepoint
  fn get_char_class(&self, c: i32) -> usize {
    let mut a = 0;
    let mut b = self.tables.points.len();
    while b - a > 1 {
      let d = (a + b) / 2;
      if self.tables.points[d] > c {
        b = d;
      } else if self.tables.points[d] < c {
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
    let char_index = c as usize;
    debug_assert!(char_index < self.alphabet_size);
    let class = if char_index >= self.tables.classmap.len() {
      self.get_char_class(c)
    } else {
      usize::from(self.tables.classmap[char_index])
    };
    self.tables.transitions[state as usize * self.tables.points.len() + class]
  }
}
impl fmt::Display for RunAutomaton {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    writeln!(f, "initial state: 0")?;
    for i in 0..self.size {
      write!(f, "state {i}")?;
      match self.tables.accept.get(i) {
        Ok(true) => write!(f, " [accept]:")?,
        Ok(false) => write!(f, " [reject]:")?,
        Err(_) => return Err(fmt::Error),
      }

      for j in 0..self.tables.points.len() {
        let k = self.tables.transitions[i * self.tables.points.len() + j];
        if k != -1 {
          let min = self.tables.points[j];
          let max = if j + 1 < self.tables.points.len() {
            self.tables.points[j + 1] - 1
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
    Ok(
      (std::mem::size_of_val(self.tables.as_ref()) as i64)
        .saturating_add(self.tables.automaton.ram_bytes_used()?)
        .saturating_add(self.tables.accept.ram_bytes_used()?)
        .saturating_add(size_of_slice(&self.tables.transitions))
        .saturating_add(size_of_slice(&self.tables.points))
        .saturating_add(size_of_slice(&self.tables.classmap)),
    )
  }
}
impl Hash for RunAutomaton {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    self.alphabet_size.hash(state);
    self.size.hash(state);
    self.tables.points.hash(state);
  }
}
use std::cmp::PartialEq;
use std::sync::Arc;

impl PartialEq for RunAutomaton {
  fn eq(&self, other: &Self) -> bool {
    if std::ptr::eq(self, other) {
      return true;
    }
    self.alphabet_size == other.alphabet_size
      && self.size == other.size
      && (Arc::ptr_eq(&self.tables, &other.tables)
        || (self.tables.points == other.tables.points
          && self.tables.accept == other.tables.accept
          && self.tables.transitions == other.tables.transitions))
  }
}

impl Eq for RunAutomaton {}
