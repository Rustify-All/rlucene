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
use crate::core::internal::hppc::bit_mixer::BitMixer;
use crate::core::util::accountable::Accountable;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::automation::automaton::Automaton;
use crate::core::util::automation::byte_runnable::ByteRunnable;
use crate::core::util::automation::int_set::IntSet;
use crate::core::util::automation::operations::PointTransitionSet;
use crate::core::util::automation::state_set::StateSet;
use crate::core::util::automation::transition::Transition;
use crate::core::util::automation::transition_accessor::TransitionAccessor;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::ram_usage_estimator::{size_of_hash_map, size_of_slice, size_of_vec};
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::hash::{Hash, Hasher};
use std::mem;
use std::sync::Arc;

/// A [`RunAutomaton`](crate::core::util::automation::run_automaton::RunAutomaton)
/// that does not require a precomputed DFA. It will lazily determinize
/// on-demand, memorizing the DFA states that have been explored.
///
/// **Note:** The current implementation is **not thread-safe**.
///
/// Implemented based on: <https://swtch.com/~rsc/regexp/regexp1.html>
#[derive(Clone)]
pub struct NFARunAutomaton {
  pub(crate) tables: Arc<NFARunAutomatonTables>,
  alphabet_size: i32,
  dstates: Vec<DState>,
  dstate_slots: usize,
  state: State,
  states_set: StateSet,
}
pub(crate) struct NFARunAutomatonTables {
  pub(crate) automaton: Automaton,
  points: Box<[i32]>,
  classmap: Box<[u16]>, // map from char number to class
}

#[derive(Clone)]
struct State {
  dstate_to_ord: HashMap<DStateKey, i32>,
  transition_set: PointTransitionSet, // reusable
}
impl NFARunAutomaton {
  const MISSING: i32 = -1;
  const NOT_COMPUTED: i32 = -2;
  pub fn new(automaton: Automaton) -> Result<Self> {
    Self::with_alphabet_size(automaton, 0x10FFFF + 1)
  }
  pub fn with_alphabet_size(automaton: Automaton, alphabet_size: i32) -> Result<Self> {
    let points = automaton.get_start_points();
    let classmap_len = std::cmp::min(256, alphabet_size) as usize;
    let mut classmap = vec![0; classmap_len];
    let mut i = 0;
    for (j, class) in classmap.iter_mut().enumerate() {
      if i + 1 < points.len() && j as i32 == points[i + 1] {
        i += 1;
      }
      // At most 256 entries, each increasing the class by at most one.
      *class = i as u16;
    }

    let state = State {
      dstate_to_ord: HashMap::new(),
      transition_set: PointTransitionSet::new(),
    };

    let mut automaton_instance = NFARunAutomaton {
      tables: Arc::new(NFARunAutomatonTables {
        automaton,
        points: points.into_boxed_slice(),
        classmap: classmap.into_boxed_slice(),
      }),
      alphabet_size,
      dstates: Vec::with_capacity(10),
      dstate_slots: 0,
      state,
      states_set: StateSet::new(5),
    };

    automaton_instance.find_dstate(Some(Arc::new(vec![0])))?;

    Ok(automaton_instance)
  }
  /// Runs through a given codepoint array and returns whether it is accepted
  /// by the automaton. This should only be used in tests.
  ///
  /// Parameters:
  /// - `s`: A string represented by an array of Unicode codepoints (`i32`)
  ///
  /// Returns:
  /// - `true` if the input is accepted; `false` otherwise.
  pub fn run(&mut self, input: &[i32]) -> Result<bool> {
    let mut p = 0;
    for &c in input {
      p = self.step(p, c)?;
      if p == Self::MISSING {
        return Ok(false);
      }
    }

    Ok(self.dstates[p as usize].is_accept)
  }
  /// From an existing DFA state, steps to the next DFA state given character
  /// `c`.
  ///
  /// If the transition was previously computed, this operation will use the
  /// cached result; otherwise, it will call [`Self::step_with_index`] to
  /// compute the next state and then cache it.
  fn step_with_dstate_index(&mut self, dstate_index: usize, c: i32) -> Result<i32> {
    let char_class = self.get_char_class(c);
    self.next_state(char_class, dstate_index)
  }
  /// return the ordinal of given DFA state, generate a new ordinal if the
  /// given DFA state is a new one
  fn find_dstate(&mut self, nfa_states: Option<Arc<Vec<i32>>>) -> Result<i32> {
    match nfa_states {
      Some(nfa_states) => {
        let hash_code = nfa_states
          .iter()
          .fold(nfa_states.len() as u32, |hash, &state| {
            hash.wrapping_add(BitMixer::mix_i32(state))
          });
        let dstate_key = DStateKey {
          nfa_states,
          hash_code: hash_code as i32,
        };
        let ord = self.state.dstate_to_ord.len();
        let nfa_states = match self.state.dstate_to_ord.entry(dstate_key) {
          Entry::Occupied(entry) => return Ok(*entry.get()),
          Entry::Vacant(entry) => {
            debug_assert!(ord <= i32::MAX as usize);
            let nfa_states = Arc::clone(&entry.key().nfa_states);
            entry.insert(ord as i32);
            nfa_states
          },
        };

        // Existing state sets need no duplicate transition table or candidate state.
        let dstate = DState::new(nfa_states, self);
        if ord >= self.dstate_slots {
          // ByteRunnable::get_size exposes the historical slot growth, separately
          // from the number of initialized states.
          let capacity = self.dstates.capacity().min(ArrayUtil::MAX_ARRAY_LENGTH);
          let slots = if capacity > ord {
            capacity
          } else {
            ArrayUtil::oversize(ord + 1, size_of::<DState>())?
          };
          self.dstates.reserve(slots - self.dstates.len());
          self.dstate_slots = slots;
        }

        debug_assert_eq!(ord, self.dstates.len());
        self.dstates.push(dstate);

        Ok(ord as i32)
      },
      None => Ok(Self::MISSING),
    }
  }
  /// Gets character class of given codepoint
  pub(crate) fn get_char_class(&self, c: i32) -> usize {
    debug_assert!(c < self.alphabet_size);

    if let Some(&class) = self.tables.classmap.get(c as usize) {
      return usize::from(class);
    }

    // binary search
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
  fn set_transition_accordingly(&self, t: &mut Transition) -> Result<()> {
    let transition_upto = t
      .transition_upto
      .ok_or_else(|| LuceneError::illegal_state("transition cursor is not initialized"))?;
    let state = &self.dstates[t.source as usize];
    t.dest = state.transitions[transition_upto];
    t.min = self.tables.points[transition_upto];

    if transition_upto == self.tables.points.len() - 1 {
      t.max = self.alphabet_size - 1;
    } else {
      t.max = self.tables.points[transition_upto + 1] - 1;
    }
    Ok(())
  }
  fn next_state(&mut self, char_class: usize, index: usize) -> Result<i32> {
    let v = {
      let len = self.tables.points.len();
      let dstate = &mut self.dstates[index];
      dstate.init_transitions(len);
      debug_assert!(char_class < dstate.transitions.len());
      dstate.transitions[char_class]
    };
    if v == NFARunAutomaton::NOT_COMPUTED {
      let next_dstate = self.step_with_index(self.tables.points[char_class], index)?;
      let ord = self.find_dstate(next_dstate)?;
      let dstate = &mut self.dstates[index];
      dstate.assign_transition(char_class, ord);
      // we could potentially update more than one char classes
      if let Some(minimal_transition) = dstate.minimal_transition.take() {
        let mut cls = char_class;
        while cls > 0 && self.tables.points[cls - 1] >= minimal_transition.min {
          cls -= 1;
          debug_assert!(
            dstate.transitions[cls] == NFARunAutomaton::NOT_COMPUTED
              || dstate.transitions[cls] == dstate.transitions[char_class]
          );
          dstate.assign_transition(cls, dstate.transitions[char_class]);
        }

        let mut cls = char_class;
        {
          while cls + 1 < self.tables.points.len()
            && self.tables.points[cls + 1] <= minimal_transition.max
          {
            cls += 1;
            debug_assert!(
              dstate.transitions[cls] == NFARunAutomaton::NOT_COMPUTED
                || dstate.transitions[cls] == dstate.transitions[char_class]
            );
            dstate.assign_transition(cls, dstate.transitions[char_class]);
          }
        }
      }
    }
    Ok(self.dstates[index].transitions[char_class])
  }
  ///  given a list of NFA states and a character c, compute the output list
  /// of NFA state which is wrapped as a DFA state
  fn step_with_index(&mut self, c: i32, index: usize) -> Result<Option<Arc<Vec<i32>>>> {
    self.states_set.reset();

    let dstate = &mut self.dstates[index];
    let nfa_states = &dstate.nfa_states;
    let mut left = -1;
    let mut right = self.alphabet_size;
    let step_transition = &mut dstate.step_transition;

    for &nfa_state in nfa_states.iter() {
      let num_transitions = self
        .tables
        .automaton
        .init_transition(nfa_state, step_transition);

      for _ in 0..num_transitions {
        self.tables.automaton.get_next_transition(step_transition)?;

        if (step_transition.min..=step_transition.max).contains(&c) {
          self.states_set.incr(step_transition.dest);
          left = left.max(step_transition.min);
          right = right.min(step_transition.max);
        }

        if step_transition.max < c {
          left = left.max(step_transition.max + 1);
        }

        if step_transition.min > c {
          right = right.min(step_transition.min - 1);
          break; // transitions are sorted
        }
      }
    }
    let next_states = if self.states_set.size() == 0 {
      None
    } else {
      self.dstates[index].minimal_transition = Some(Transition {
        min: left,
        max: right,
        ..Default::default()
      });
      Some(self.states_set.get_array().clone())
    };

    Ok(next_states)
  }
  fn determinize(&mut self, index: usize) -> Result<()> {
    let len = self.tables.points.len();
    let dstate = &mut self.dstates[index];
    if dstate.computed_transitions == len {
      return Ok(());
    }
    dstate.init_transitions(len);
    self.state.transition_set.reset();

    for &nfa_state in dstate.nfa_states.iter() {
      let num_transitions = self
        .tables
        .automaton
        .init_transition(nfa_state, &mut dstate.step_transition);
      for _ in 0..num_transitions {
        self
          .tables
          .automaton
          .get_next_transition(&mut dstate.step_transition)?;
        self.state.transition_set.add(&dstate.step_transition)?;
      }
    }

    {
      if self.state.transition_set.count == 0 {
        dstate.transitions.fill(NFARunAutomaton::MISSING);
        dstate.computed_transitions = dstate.transitions.len();
        return Ok(());
      }
    }
    self.state.transition_set.sort()?;
    self.states_set.reset();
    let mut last_point = -1;
    let mut char_class = 0;

    let count = self.state.transition_set.count;
    for i in 0..count {
      let point = self.state.transition_set.points[i].point;

      if self.states_set.size() > 0 {
        debug_assert_ne!(last_point, -1);

        let v = self.states_set.get_array().clone();
        let ord = self.find_dstate(Some(v))?;
        let dstate = &mut self.dstates[index];

        while self.tables.points[char_class] < last_point {
          dstate.assign_transition(char_class, Self::MISSING);
          char_class += 1;
        }

        debug_assert_eq!(self.tables.points[char_class], last_point);

        while char_class < self.tables.points.len() && self.tables.points[char_class] < point {
          debug_assert!(
            dstate.transitions[char_class] == NFARunAutomaton::NOT_COMPUTED
              || dstate.transitions[char_class] == ord
          );
          dstate.assign_transition(char_class, ord);
          char_class += 1;
        }

        debug_assert!(
          (char_class == self.tables.points.len() && point == self.alphabet_size)
            || self.tables.points[char_class] == point
        );
      }

      // process transitions that end on this point
      // (closes an overlapping interval)
      let ends = &mut self.state.transition_set.points[i].ends;
      let limit = ends.next;
      for j in (0..limit).step_by(3) {
        let dest = ends.transitions[j];
        self.states_set.decr(dest)?;
      }
      ends.next = 0;

      // process transitions that start on this point
      // (opens a new interval)
      let starts = &mut self.state.transition_set.points[i].starts;
      let limit = starts.next;
      for j in (0..limit).step_by(3) {
        let dest = starts.transitions[j];
        self.states_set.incr(dest);
      }

      last_point = point;
      starts.next = 0;
    }
    let dstate = &mut self.dstates[index];
    debug_assert_eq!(self.states_set.size(), 0);
    debug_assert!(dstate.computed_transitions >= char_class);
    // it's also possible that some transitions after the charClass has already
    // been explored
    // no more outgoing transitions, set rest of transition to MISSING
    if char_class < dstate.transitions.len() {
      debug_assert!(
        dstate.transitions[char_class] == NFARunAutomaton::MISSING
          || dstate.transitions[char_class] == NFARunAutomaton::NOT_COMPUTED
      );
    }

    let len = dstate.transitions.len();
    dstate.transitions[char_class..len].fill(NFARunAutomaton::NOT_COMPUTED);

    dstate.computed_transitions = dstate.transitions.len();
    Ok(())
  }

  pub fn init_transition(&mut self, state: i32, t: &mut Transition) -> Result<i32> {
    t.source = state;
    t.transition_upto = None;
    self.get_num_transitions_with_state(state)
  }

  pub fn get_next_transition(&self, t: &mut Transition) -> Result<()> {
    debug_assert!(t.transition_upto.map_or(0, |upto| upto + 1) < self.tables.points.len());
    {
      let transitions = &self.dstates[t.source as usize].transitions;
      let transition_upto = loop {
        // this shouldn't return AIOOBE as long as this function is only called
        // numTransitions times
        let idx = t.transition_upto.map_or(0, |upto| upto + 1);
        t.transition_upto = Some(idx);
        if transitions[idx] != Self::MISSING {
          break idx;
        }
      };

      debug_assert!(transitions[transition_upto] != Self::NOT_COMPUTED);
    }
    self.set_transition_accordingly(t)
  }

  pub fn get_num_transitions_with_state(&mut self, state: i32) -> Result<i32> {
    let state_index = state as usize;
    self.determinize(state_index)?;
    Ok(self.dstates[state_index].outgoing_transitions)
  }

  pub fn get_transition(&mut self, state: i32, index: i32, t: &mut Transition) -> Result<()> {
    let state_index = state as usize;
    self.determinize(state_index)?;
    {
      let transitions = &self.dstates[state_index].transitions;

      let mut outgoing_transitions = -1;
      t.transition_upto = None;
      t.source = state;

      while outgoing_transitions < index
        && t.transition_upto.map_or(0, |upto| upto + 1) < self.tables.points.len()
      {
        let idx = t.transition_upto.map_or(0, |upto| upto + 1);
        t.transition_upto = Some(idx);
        if transitions[idx] != Self::MISSING {
          outgoing_transitions += 1;
        }
      }

      debug_assert_eq!(outgoing_transitions, index);
    }
    self.set_transition_accordingly(t)?;
    Ok(())
  }
}
impl ByteRunnable for NFARunAutomaton {
  /// For a given state and an incoming character (codepoint), returns the
  /// next state.
  ///
  /// Parameters:
  /// - `state`: The incoming state. It should either be `0` or a state
  ///   previously returned by this function.
  /// - `c`: The Unicode codepoint to transition on
  ///
  /// Returns:
  /// - The next state, or `Self::MISSING` if the transition doesn't exist.
  fn step(&mut self, state: i32, c: i32) -> Result<i32> {
    let state_index = state as usize;
    debug_assert!(self.dstates.get(state_index).is_some());
    self.step_with_dstate_index(state_index, c)
  }

  fn is_accept(&self, state: i32) -> Result<bool> {
    let state_index = state as usize;
    debug_assert!(self.dstates.get(state_index).is_some());
    Ok(self.dstates[state_index].is_accept)
  }

  fn get_size(&self) -> usize {
    debug_assert!(self.dstates.len() <= i32::MAX as usize);
    self.dstate_slots
  }
}

impl Accountable for NFARunAutomaton {
  fn ram_bytes_used(&self) -> Result<i64> {
    let mut size = (mem::size_of_val(self.tables.as_ref()) as i64)
      .saturating_add(self.tables.automaton.ram_bytes_used()?)
      .saturating_add(size_of_slice(&self.tables.points))
      .saturating_add(size_of_slice(&self.tables.classmap))
      .saturating_add(size_of_vec(&self.dstates))
      .saturating_add(size_of_hash_map(&self.state.dstate_to_ord));

    for dstate in &self.dstates {
      size = size.saturating_add(dstate.ram_bytes_used()?);
    }

    Ok(size)
  }
}

#[derive(Clone)]
struct DState {
  nfa_states: Arc<Vec<i32>>,
  transitions: Vec<i32>,
  is_accept: bool,
  step_transition: Transition,
  minimal_transition: Option<Transition>,
  computed_transitions: usize,
  outgoing_transitions: i32,
}
impl DState {
  fn new(nfa_states: Arc<Vec<i32>>, nfa: &NFARunAutomaton) -> Self {
    debug_assert!(!nfa_states.is_empty());

    debug_assert!(nfa_states.len() <= i32::MAX as usize);
    let mut is_accept = false;

    for s in nfa_states.iter() {
      if nfa.tables.automaton.is_accept(*s) {
        is_accept = true;
      }
    }
    DState {
      nfa_states,
      transitions: Vec::new(),
      is_accept,
      step_transition: Transition::default(),
      minimal_transition: None,
      computed_transitions: 0,
      outgoing_transitions: 0,
    }
  }

  fn assign_transition(&mut self, char_class: usize, dest: i32) {
    if self.transitions[char_class] == NFARunAutomaton::NOT_COMPUTED {
      self.computed_transitions += 1;
      self.transitions[char_class] = dest;
      if dest != NFARunAutomaton::MISSING {
        self.outgoing_transitions += 1;
      }
    }
  }

  fn init_transitions(&mut self, points_len: usize) {
    if self.transitions.len() < points_len {
      self
        .transitions
        .reserve_exact(points_len - self.transitions.len());
      self
        .transitions
        .resize(points_len, NFARunAutomaton::NOT_COMPUTED);
    }
  }
}

impl Accountable for DState {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(
      (mem::size_of_val(self.nfa_states.as_ref()) as i64)
        .saturating_add(size_of_vec(self.nfa_states.as_ref()))
        .saturating_add(size_of_vec(&self.transitions)),
    )
  }
}

#[derive(Clone)]
struct DStateKey {
  nfa_states: Arc<Vec<i32>>,
  hash_code: i32,
}
impl PartialEq for DStateKey {
  fn eq(&self, other: &Self) -> bool {
    self.hash_code == other.hash_code && self.nfa_states == other.nfa_states
  }
}
impl Eq for DStateKey {}
impl Hash for DStateKey {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    self.hash_code.hash(state);
  }
}
