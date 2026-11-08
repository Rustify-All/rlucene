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
use bit_set::BitSet;
use smallvec::SmallVec;
use std::borrow::Borrow;
use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::fmt;
use std::fmt::{Display, Formatter};

use crate::core::index::{BytesRef, BytesRefBuilder};
use crate::core::internal::hppc::bit_mixer::BitMixer;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::util::BitSetExt;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::automation::automata::Automata;
use crate::core::util::automation::automaton::{Automaton, Builder};
use crate::core::util::automation::state_pair::StatePair;
use crate::core::util::automation::state_set::StateSet;
use crate::core::util::automation::transition::Transition;
use crate::core::util::automation::transition_accessor::TransitionAccessor;
use crate::core::util::bit_set::BitSet as OtherBitSet;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::ints_ref::IntsRef;
use crate::core::util::ints_ref_builder::IntsRefBuilder;

/// Automata operations.
pub struct Operations;
impl Operations {
  /// Default maximum effort that [`Operations::determinize`] should spend
  /// before giving up and returning
  /// [`TooComplexToDeterminizeError`](crate::core::util::error::TooComplexToDeterminizeError).
  pub const DEFAULT_DETERMINIZE_WORK_LIMIT: usize = 10000;
  /// Returns an automaton that accepts the concatenation of the languages of
  /// the given automata.
  ///
  /// Complexity: linear in the total number of states.
  pub fn concatenate(a1: &Automaton, a2: &Automaton) -> Result<Automaton> {
    Operations::concatenate_with_list(&[a1, a2])
  }
  /// Returns an automaton that accepts the concatenation of the languages of
  /// the given automata.
  ///
  /// Complexity: linear in the total number of states.
  pub fn concatenate_with_list<A: Borrow<Automaton>>(l: &[A]) -> Result<Automaton> {
    let mut result = Automaton::new();
    // First pass: create all states
    for a in l {
      let a = a.borrow();
      if a.get_num_states() == 0 {
        result.finish_state()?;
        return Ok(result);
      }
      let num_states = a.get_num_states();
      for _ in 0..num_states {
        result.create_state()?;
      }
    }

    // Second pass: add transitions, linking accept states of each automaton to the
    // start of the next
    let mut state_offset = 0;
    let mut t = Transition::default();
    for i in 0..l.len() {
      let a = l[i].borrow();
      let num_states = a.get_num_states();
      let next_a = if i + 1 < l.len() {
        Some(l[i + 1].borrow())
      } else {
        None
      };

      for s in 0..num_states {
        let count = a.init_transition(s, &mut t);
        for _ in 0..count {
          a.get_next_transition(&mut t)?;
          result.add_transition(state_offset + s, state_offset + t.dest, t.min, t.max)?;
        }

        if a.is_accept(s) {
          let mut follow_offset = state_offset;
          let mut upto = i + 1;
          let mut follow_a = next_a;

          loop {
            if let Some(fa) = follow_a {
              let num_transitions = fa.init_transition(0, &mut t);
              for _ in 0..num_transitions {
                fa.get_next_transition(&mut t)?;
                result.add_transition(
                  state_offset + s,
                  follow_offset + num_states + t.dest,
                  t.min,
                  t.max,
                )?;
              }
              if fa.is_accept(0) {
                follow_offset += fa.get_num_states();
                if upto + 1 < l.len() {
                  follow_a = Some(l[upto + 1].borrow());
                } else {
                  follow_a = None;
                }
                upto += 1;
              } else {
                break;
              }
            } else {
              result.set_accept(state_offset + s, true);
              break;
            }
          }
        }
      }

      state_offset += num_states;
    }

    if result.get_num_states() == 0 {
      result.create_state()?;
    }
    result.finish_state()?;
    Ok(result)
  }
  /// Returns an automaton that accepts the union of the empty string and the
  /// language of the given automaton. This may create a dead state.
  ///
  /// Complexity: linear in the number of states.
  pub fn optional(a: &Automaton) -> Result<Cow<'_, Automaton>> {
    // If the initial state already accepts, return as is
    if a.is_accept(0) {
      return Ok(Cow::Borrowed(a));
    }

    // Check for any transition back to the initial state
    let mut has_transitions_to_initial = false;
    let mut t = Transition::default();
    'outer: for s in 0..a.get_num_states() {
      let count = a.init_transition(s, &mut t);
      for _ in 0..count {
        a.get_next_transition(&mut t)?;
        if t.dest == 0 {
          has_transitions_to_initial = true;
          break 'outer;
        }
      }
    }

    // If no transitions to initial, just mark initial as accept
    if !has_transitions_to_initial {
      let mut result = Automaton::new();
      result.copy(a)?;
      if result.get_num_states() == 0 {
        result.create_state()?;
      }
      result.set_accept(0, true);
      return Ok(Cow::Owned(result));
    }
    let mut result = Automaton::new();
    result.create_state()?;
    result.set_accept(0, true);
    if a.get_num_states() > 0 {
      result.copy(a)?;
      result.add_epsilon(0, 1)?;
    }
    result.finish_state()?;
    Ok(Cow::Owned(result))
  }
  /// Returns an automaton that accepts the Kleene star (zero or more
  /// concatenated repetitions) of the language of the given automaton.
  /// Never modifies the input automaton language.
  ///
  /// Complexity: linear in the number of states.
  pub fn repeat(a: &Automaton) -> Result<Cow<'_, Automaton>> {
    if a.get_num_states() == 0 {
      // Repeating the empty automata will still only accept the empty automata.
      return Ok(Cow::Borrowed(a));
    }

    // If state 0 is the only accept state, and it already repeats itself
    if a.is_accept(0) && a.get_accept_states().count() == 1 {
      return Ok(Cow::Borrowed(a));
    }

    let mut builder = Builder::new();
    builder.create_state(); // initial state
    builder.set_accept(0, true);

    let num_states = a.get_num_states();
    let mut state_map = Vec::with_capacity(num_states as usize);
    for state in 0..num_states {
      let mapped = if !a.is_accept(state) {
        builder.create_state()
      } else if a.get_num_transitions_with_state(state) == 0 {
        0 // merge into initial state
      } else {
        let new_state = builder.create_state();
        builder.set_accept(new_state, true);
        new_state
      };
      state_map.push(mapped);
    }

    // Copy transitions with remapped states
    let mut t = Transition::default();
    for state in 0..a.get_num_states() {
      let src = state_map[state as usize];
      let count = a.init_transition(state, &mut t);
      for _ in 0..count {
        a.get_next_transition(&mut t)?;
        let dest = state_map[t.dest as usize];
        builder.add_transition(src, dest, t.min, t.max)?;
      }
    }

    // Copy initial transitions to new initial state (state 0)
    let count = a.init_transition(0, &mut t);
    for _ in 0..count {
      a.get_next_transition(&mut t)?;
      builder.add_transition(0, state_map[t.dest as usize], t.min, t.max)?;
    }

    // Add transitions from each accept state to repeat the initial transitions
    let accept_set = a.get_accept_states();
    for s in accept_set.iter() {
      if state_map[s] != 0 {
        let count = a.init_transition(0, &mut t);
        for _ in 0..count {
          a.get_next_transition(&mut t)?;
          builder.add_transition(state_map[s], state_map[t.dest as usize], t.min, t.max)?;
        }
      }
    }
    let automaton = builder.finish()?;
    let v = Operations::remove_dead_states(&automaton)?;
    match v {
      Cow::Borrowed(_) => Ok(Cow::Owned(automaton)),
      Cow::Owned(o) => Ok(Cow::Owned(o)),
    }
  }
  /// Returns an automaton that accepts `min` or more concatenated repetitions
  /// of the language of the given automaton.
  ///
  /// Complexity: linear in the number of states and in `min`.
  pub fn repeat_count(a: &Automaton, count: i32) -> Result<Cow<'_, Automaton>> {
    if count == 0 {
      return Operations::repeat(a);
    }

    // The borrowed repetition must outlive the list, including its destructor.
    #[expect(
      clippy::needless_late_init,
      reason = "preserve initialization order while letting the borrowed value outlive SmallVec"
    )]
    let repeated;
    let mut automata: SmallVec<[_; 8]> = SmallVec::with_capacity(count as usize + 1);
    for _ in 0..count {
      automata.push(a);
    }
    repeated = Operations::repeat(a)?;
    automata.push(&repeated);

    Ok(Cow::Owned(Operations::concatenate_with_list(&automata)?))
  }
  /// Returns an automaton that accepts between `min` and `max` (inclusive)
  /// concatenated repetitions of the language of the given automaton.
  ///
  /// Complexity: linear in the number of states, `min`, and `max`.
  pub fn repeat_min_max(a: &Automaton, min: i32, max: i32) -> Result<Automaton> {
    if min > max {
      return Automata::make_empty();
    }

    let b = if min == 0 {
      Automata::make_empty_string()?
    } else if min == 1 {
      let mut base = Automaton::new();
      base.copy(a)?;
      base
    } else {
      let min = min as usize;
      let mut reps: SmallVec<[_; 8]> = SmallVec::with_capacity(min);
      for _ in 0..min {
        reps.push(a);
      }
      Operations::concatenate_with_list(&reps)?
    };

    let mut prev_accept = Operations::get_set(&b, 0);
    let mut builder = Builder::new();
    builder.copy(&b)?;

    for _ in min..max {
      let offset = builder.get_num_states();
      builder.copy(a)?;
      for s in prev_accept.iter() {
        builder.add_epsilon(*s, offset)?;
      }
      prev_accept = Operations::get_set(a, offset);
    }

    builder.finish()
  }
  fn get_set(a: &Automaton, offset: i32) -> HashSet<i32> {
    let mut result = HashSet::new();
    for s in 0..a.get_num_states() {
      if a.is_accept(s) {
        result.insert(offset + s);
      }
    }
    result
  }
  /// Returns a (deterministic) automaton that accepts the complement of the
  /// language of the given automaton.
  ///
  /// Complexity: linear in the number of states if already deterministic, and
  /// exponential otherwise.
  ///
  /// Parameters:
  /// - `determinize_work_limit`: Maximum effort to spend determinizing the
  ///   automaton. Set higher to allow more complex queries and lower to
  ///   prevent memory exhaustion.
  ///   [`DEFAULT_DETERMINIZE_WORK_LIMIT`](Self::DEFAULT_DETERMINIZE_WORK_LIMIT)
  ///   is a good starting default.
  pub fn complement(a: &Automaton, determinize_work_limit: usize) -> Result<Automaton> {
    let v = Operations::determinize(a, determinize_work_limit)?;
    let mut a = Operations::totalize(&v)?;

    let num_states = a.get_num_states();
    for p in 0..num_states {
      let is_accept = a.is_accept(p);
      a.set_accept(p, !is_accept);
    }

    match Operations::remove_dead_states(&a)? {
      Cow::Borrowed(_) => Ok(a),
      Cow::Owned(o) => Ok(o),
    }
  }
  /// Returns a (deterministic) automaton that accepts the intersection of the
  /// language of `a1` and the complement of the language of `a2`.
  /// As a side-effect, the automata may be determinized if not already
  /// deterministic.
  ///
  /// Complexity: quadratic in the number of states if `a2` is already
  /// deterministic, and exponential in the number of `a2`'s states
  /// otherwise.
  ///
  /// Parameters:
  /// - `a1`: The initial automaton
  /// - `a2`: The automaton to subtract
  /// - `determinize_work_limit`: Maximum effort to spend determinizing the
  ///   automaton. Set higher to allow more complex queries and lower to
  ///   prevent memory exhaustion.
  ///   [`DEFAULT_DETERMINIZE_WORK_LIMIT`](Self::DEFAULT_DETERMINIZE_WORK_LIMIT)
  ///   is a good starting default.
  pub fn minus<'a>(
    a1: &'a Automaton,
    a2: &'a Automaton,
    determinize_work_limit: usize,
  ) -> Result<Cow<'a, Automaton>> {
    if Operations::is_empty(a1)? || std::ptr::eq(a1, a2) {
      return Ok(Cow::Owned(Automata::make_empty()?));
    }
    if Operations::is_empty(a2)? {
      return Ok(Cow::Borrowed(a1));
    }
    let complement_a2 = Operations::complement(a2, determinize_work_limit)?;

    match Operations::intersection(a1, &complement_a2)? {
      Cow::Borrowed(v) if std::ptr::eq(v, &complement_a2) => Ok(Cow::Owned(complement_a2)),
      Cow::Owned(o) => Ok(Cow::Owned(o)),
      _ => Ok(Cow::Borrowed(a1)),
    }
  }
  /// Returns an automaton that accepts the intersection of the languages of
  /// the given automata. Never modifies the input automata languages.
  ///
  /// Complexity: quadratic in the number of states.
  pub fn intersection<'a>(a1: &'a Automaton, a2: &'a Automaton) -> Result<Cow<'a, Automaton>> {
    if std::ptr::eq(a1, a2) {
      return Ok(Cow::Borrowed(a1));
    }
    if a1.get_num_states() == 0 {
      return Ok(Cow::Borrowed(a1));
    }
    if a2.get_num_states() == 0 {
      return Ok(Cow::Borrowed(a2));
    }

    let mut c = Automaton::new();
    c.create_state()?;

    let mut worklist = VecDeque::new();
    let mut newstates = HashMap::new();

    let p = StatePair::with_s(0, 0, 0);
    worklist.push_back(p.clone());
    let state = p.s;
    newstates.insert(p, state);

    // Read each visited row from the inputs instead of copying both transition tables.
    let mut t1 = Transition::default();
    let mut t2 = Transition::default();
    while let Some(p) = worklist.pop_front() {
      c.set_accept(p.s, a1.is_accept(p.s1) && a2.is_accept(p.s2));

      let count1 = a1.get_num_transitions_with_state(p.s1);
      let count2 = a2.get_num_transitions_with_state(p.s2);

      let mut b2 = 0;

      for n1 in 0..count1 {
        a1.get_transition(p.s1, n1, &mut t1);
        while b2 < count2 {
          a2.get_transition(p.s2, b2, &mut t2);
          if t2.max >= t1.min {
            break;
          }
          b2 += 1;
        }
        let mut n2 = b2;
        while n2 < count2 {
          a2.get_transition(p.s2, n2, &mut t2);
          if t1.max < t2.min {
            break;
          }
          if t2.max >= t1.min {
            let mut q = StatePair::new(t1.dest, t2.dest);
            let r = match newstates.get(&q) {
              Some(&r) => r,
              None => {
                q.s = c.create_state()?;
                worklist.push_back(q.clone());
                let state = q.s;
                newstates.insert(q, state);
                state
              },
            };

            let min = t1.min.max(t2.min);
            let max = t1.max.min(t2.max);

            c.add_transition(p.s, r, min, max)?;
          }
          n2 += 1;
        }
      }
    }

    c.finish_state()?;
    match Operations::remove_dead_states(&c)? {
      Cow::Borrowed(_) => Ok(Cow::Owned(c)),
      Cow::Owned(o) => Ok(Cow::Owned(o)),
    }
  }
  /// Returns `true` if this automaton has any states that cannot be reached
  /// from the initial state or cannot reach an accept state.
  ///
  /// Cost: O(num_transitions + num_states).
  pub fn has_dead_states(a: &Automaton) -> Result<bool> {
    let live_states = Operations::get_live_states(a)?;
    let num_live = live_states.count();
    let num_states = a.get_num_states() as usize;
    debug_assert!(
      num_live <= num_states,
      "num_live = {num_live}, num_states = {num_states}, live = {live_states:?}"
    );
    Ok(num_live < num_states)
  }
  ///  Returns true if there are dead states reachable from an initial state.
  pub fn has_dead_states_from_initial(a: &Automaton) -> Result<bool> {
    let mut reachable_from_initial = Operations::get_live_states_from_initial(a)?;
    let reachable_from_accept = Operations::get_live_states_to_accept(a)?;

    reachable_from_initial.difference_with(&reachable_from_accept);

    Ok(!reachable_from_initial.is_empty())
  }
  /// Returns true if there are dead states that reach an accept state.
  pub fn has_dead_states_to_accept(a: &Automaton) -> Result<bool> {
    let reachable_from_initial = Operations::get_live_states_from_initial(a)?;
    let mut reachable_from_accept = Operations::get_live_states_to_accept(a)?;
    reachable_from_accept.difference_with(&reachable_from_initial);
    Ok(!reachable_from_accept.is_empty())
  }
  /// Returns an automaton that accepts the union of the languages of the
  /// given automata.
  ///
  /// Complexity: linear in the number of states.
  pub fn union(a1: &Automaton, a2: &Automaton) -> Result<Automaton> {
    Operations::union_list(&[a1, a2])
  }
  /// Returns an automaton that accepts the union of the languages of the
  /// given automata.
  ///
  /// Complexity: linear in the number of states.
  pub fn union_list<A: Borrow<Automaton>>(l: &[A]) -> Result<Automaton> {
    let mut result = Automaton::new();
    // Create initial state:
    result.create_state()?;

    // Copy over all automata
    for a in l {
      result.copy(a.borrow())?;
    }

    // Add epsilon transitions from new initial state
    let mut state_offset = 1;
    for a in l {
      let a = a.borrow();
      if a.get_num_states() == 0 {
        continue;
      }
      result.add_epsilon(0, state_offset)?;
      state_offset += a.get_num_states();
    }

    result.finish_state()?;
    match Operations::remove_dead_states(&result)? {
      Cow::Borrowed(_) => Ok(result),
      Cow::Owned(o) => Ok(o),
    }
  }
  /// Determinizes the given automaton.
  ///
  /// Worst case complexity: exponential in the number of states.
  ///
  /// Parameters:
  /// - `work_limit`: Maximum amount of "work" that the powerset construction
  ///   will spend before returning an error. Higher numbers allow this
  ///   operation to consume more memory and CPU but allow more complex
  ///   automata. [`DEFAULT_DETERMINIZE_WORK_LIMIT`](Self::DEFAULT_DETERMINIZE_WORK_LIMIT)
  ///   is a good starting default if you don't otherwise know what to
  ///   specify.
  ///
  /// Errors:
  /// - Returns [`TooComplexToDeterminizeError`](crate::core::util::error::TooComplexToDeterminizeError) if determinizing requires
  ///   more than `work_limit` units of effort.
  pub fn determinize(a: &'_ Automaton, work_limit: usize) -> Result<Cow<'_, Automaton>> {
    if a.is_deterministic() || a.get_num_states() <= 1 {
      return Ok(Cow::Borrowed(a));
    }

    let mut b = Builder::new();

    b.create_state();

    // All frozen sets belong to this determinization. State ordinals index their ranges;
    // the second field links distinct sets with the same long hash code.
    let mut state_values = vec![0];
    let mut state_sets: Vec<(std::ops::Range<usize>, Option<i32>)> = vec![(0..1, None)];
    let mut worklist = VecDeque::new();
    let mut newstate = HashMap::new();

    newstate.insert(BitMixer::mix_i32(0) as i64 + 1, 0);
    worklist.push_back(0);
    b.set_accept(0, a.is_accept(0));

    let mut points = PointTransitionSet::new();
    let mut states_set = StateSet::with_owned_array(5);

    let mut t = Transition::default();

    let mut effort_spent: u64 = 0;
    let effort_limit: u64 = (work_limit as u64) * 10;

    while let Some(r) = worklist.pop_front() {
      let range = state_sets[r as usize].0.clone();
      effort_spent += range.len() as u64;
      if effort_spent >= effort_limit {
        return Err(LuceneError::too_complex_to_determinize(format!(
          "Determinizing automaton with {}, states and {} transitions would require more than {} effort.",
          a.get_num_states(),
          a.get_num_transitions(),
          work_limit
        )));
      }

      // Collate outgoing transitions:
      for &s0 in &state_values[range] {
        let num_transitions = a.get_num_transitions_with_state(s0);
        a.init_transition(s0, &mut t);
        for _ in 0..num_transitions {
          a.get_next_transition(&mut t)?;
          points.add(&t)?;
        }
      }

      if points.count == 0 {
        continue;
      }

      points.sort()?;

      let mut last_point = -1;
      let mut acc_count = 0;

      for i in 0..points.count {
        let point = points.points[i].point;
        if states_set.size() > 0 {
          debug_assert!(last_point != -1);
          let hash = states_set.long_hash_code();
          let values = states_set.get_array();
          let head = newstate.get(&hash).copied();
          let mut existing = head;
          while let Some(q) = existing {
            let (range, next) = &state_sets[q as usize];
            if state_values[range.clone()] == values[..] {
              break;
            }
            existing = *next;
          }
          let q = match existing {
            Some(q) => {
              debug_assert_eq!(
                acc_count > 0,
                b.is_accept(q),
                "accCount={} vs existing accept={}",
                acc_count,
                b.is_accept(q)
              );
              q
            },
            None => {
              let q = b.create_state();
              let start = state_values.len();
              state_values.extend_from_slice(values);
              state_sets.push((start..state_values.len(), head));
              worklist.push_back(q);
              b.set_accept(q, acc_count > 0);
              newstate.insert(hash, q);
              q
            },
          };

          b.add_transition(r, q, last_point, point - 1)?;
        }
        {
          let ends = &mut points.points[i].ends;
          let transitions = &ends.transitions;
          let limit = ends.next;
          for j in (0..limit).step_by(3) {
            let dest = transitions[j];
            states_set.decr(dest)?;
            if a.is_accept(dest) {
              acc_count -= 1;
            }
          }
          ends.next = 0;
        }

        {
          let start = &mut points.points[i].starts;
          let transitions = &start.transitions;
          let limit = start.next;
          for j in (0..limit).step_by(3) {
            let dest = transitions[j];
            states_set.incr(dest);
            if a.is_accept(dest) {
              acc_count += 1;
            }
          }
          last_point = point;
          start.next = 0;
        }
      }
      points.reset();
      debug_assert_eq!(states_set.size(), 0);
    }
    let result = b.finish()?;
    debug_assert!(result.is_deterministic());
    Ok(Cow::Owned(result))
  }
  /// Returns true if the given automaton accepts no strings.
  pub fn is_empty(a: &Automaton) -> Result<bool> {
    if a.get_num_states() == 0 {
      return Ok(true);
    }
    if !a.is_accept(0) && a.get_num_transitions_with_state(0) == 0 {
      // Common case: just one initial state
      return Ok(true);
    }

    if a.is_accept(0) {
      // Apparently common case: it accepts the damned empty string
      return Ok(false);
    }

    let mut work_list = VecDeque::new();
    let mut seen = BitSet::with_capacity(a.get_num_states() as usize);
    work_list.push_back(0);
    seen.insert(0);

    let mut t = Transition::default();

    while let Some(state) = work_list.pop_front() {
      if a.is_accept(state) {
        return Ok(false);
      }

      let count = a.init_transition(state, &mut t);
      for _ in 0..count {
        a.get_next_transition(&mut t)?;
        if seen.insert(t.dest as usize) {
          work_list.push_back(t.dest);
        }
      }
    }

    Ok(true)
  }
  /// Returns `true` if the given automaton accepts all strings.
  ///
  /// The automaton must be deterministic, otherwise this method may return
  /// `false`.
  ///
  /// Complexity: linear in the number of states and transitions.
  pub fn is_total(a: &Automaton) -> Result<bool> {
    Operations::is_total_with_range(a, char::MIN as i32, char::MAX as i32)
  }
  /// Returns `true` if the given automaton accepts all strings for the
  /// specified min/max range of the alphabet.
  ///
  /// The automaton must be deterministic, otherwise this method may return
  /// `false`.
  ///
  /// Complexity: linear in the number of states and transitions.
  pub fn is_total_with_range(a: &Automaton, min_alphabet: i32, max_alphabet: i32) -> Result<bool> {
    let states = Operations::get_live_states(a)?;
    let mut spare = Transition::default();
    let mut seen_states = 0;

    let mut state = states.next_set_bit(0);
    while state >= 0 {
      if !a.is_accept(state) {
        return Ok(false);
      }
      let mut previous_label = min_alphabet - 1;
      for t_index in 0..a.get_num_transitions_with_state(state) {
        a.get_transition(state, t_index, &mut spare);
        if spare.min > previous_label + 1 {
          return Ok(false);
        }
        previous_label = spare.max;
      }

      if previous_label < max_alphabet {
        return Ok(false);
      }

      if state == i32::MAX {
        break;
      }

      seen_states += 1;
      state = states.next_set_bit(state as usize + 1);
    }
    Ok(seen_states > 0)
  }
  /// Returns `true` if the given string is accepted by the automaton. The
  /// input must be deterministic.
  ///
  /// Complexity: linear in the length of the string.
  ///
  /// **Note:** For full performance, use the
  /// [`RunAutomaton`](crate::core::util::automation::run_automaton::RunAutomaton)
  /// struct.
  pub fn run_str(a: &Automaton, s: &str) -> bool {
    debug_assert!(a.is_deterministic());

    let mut state = 0;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.peek() {
      let cp = *c as u32 as i32;
      let next_state = a.step(state, cp);
      if next_state == -1 {
        return false;
      }
      state = next_state;
      chars.next();
    }

    a.is_accept(state)
  }

  /// Returns `true` if the given string (expressed as Unicode codepoints) is
  /// accepted by the automaton. The input must be deterministic.
  ///
  /// Complexity: linear in the length of the string.
  ///
  /// **Note:** For full performance, use the
  /// [`RunAutomaton`](crate::core::util::automation::run_automaton::RunAutomaton)
  /// struct.
  pub fn run_ints_ref(a: &Automaton, s: &IntsRef<Vec<i32>>) -> bool {
    debug_assert!(a.is_deterministic());

    let mut state = 0;
    for i in 0..s.length {
      let label = s.ints[s.offset + i];
      let next_state = a.step(state, label);
      if next_state == -1 {
        return false;
      }
      state = next_state;
    }
    a.is_accept(state)
  }
  /// Returns the set of live states.
  /// A state is considered "live" if an accept state is reachable from it
  /// and if it is reachable from the initial state.
  pub fn get_live_states(a: &Automaton) -> Result<BitSet> {
    if a.get_accept_states().is_empty() {
      return Ok(BitSet::with_capacity(a.get_num_states() as usize));
    }
    let mut live = Operations::get_live_states_from_initial(a)?;
    live.intersect_with(&Operations::get_live_states_to_accept(a)?);
    Ok(live)
  }
  /// Returns a bitset marking states reachable from the initial state.
  pub fn get_live_states_from_initial(a: &Automaton) -> Result<BitSet> {
    let num_states = a.get_num_states();
    let mut live = BitSet::with_capacity(num_states as usize);
    if num_states == 0 {
      return Ok(live);
    }
    let mut work_list = VecDeque::new();
    live.insert(0);
    work_list.push_back(0);

    let mut t = Transition::default();
    while let Some(s) = work_list.pop_front() {
      let count = a.init_transition(s, &mut t);
      for _ in 0..count {
        a.get_next_transition(&mut t)?;
        let dest = t.dest as usize;
        if live.insert(dest) {
          work_list.push_back(t.dest);
        }
      }
    }
    Ok(live)
  }
  /// Returns a bitset marking states that can reach an accept state.
  fn get_live_states_to_accept(a: &Automaton) -> Result<BitSet> {
    let num_states = a.get_num_states();
    // build reversed automaton
    let mut builder = Builder::with_capacity(num_states as usize, a.get_num_transitions() as usize);
    for _ in 0..num_states {
      builder.create_state();
    }
    let mut t = Transition::default();
    for s in 0..num_states {
      let count = a.init_transition(s, &mut t);
      for _ in 0..count {
        a.get_next_transition(&mut t)?;
        builder.add_transition(t.dest, s, t.min, t.max)?;
      }
    }
    let a2 = builder.finish()?;

    // collect accept states and traverse backwards
    let mut live = BitSet::with_capacity(num_states as usize);
    let mut work_list = VecDeque::new();
    let accept_bits = a.get_accept_states();
    let mut s = 0;
    while s < num_states {
      s = accept_bits.next_set_bit(s as usize);
      if s == -1 {
        break;
      }
      let su = s as usize;
      live.insert(su);
      work_list.push_back(s);
      s += 1;
    }
    while let Some(s) = work_list.pop_front() {
      let count = a2.init_transition(s, &mut t);
      for _ in 0..count {
        a2.get_next_transition(&mut t)?;
        let dest = t.dest as usize;
        if live.insert(dest) {
          work_list.push_back(t.dest);
        }
      }
    }
    Ok(live)
  }
  /// Removes transitions to dead states.
  /// A state is considered "dead" if it is not reachable from the initial
  /// state or if no accept state is reachable from it.
  pub fn remove_dead_states(a: &'_ Automaton) -> Result<Cow<'_, Automaton>> {
    let num_states = a.get_num_states();
    let state_count = num_states as usize;
    let live_set = Operations::get_live_states(a)?;
    let live_count = live_set.count();
    if live_count == state_count {
      return Ok(Cow::Borrowed(a));
    }

    let mut result = Automaton::with_capacity(live_count.max(2), 2);
    if live_count == 0 {
      return Ok(Cow::Owned(result));
    }
    let mut map = vec![0; state_count];

    for (i, state) in (0..num_states).enumerate() {
      if live_set.contains(i) {
        let s = result.create_state()?;
        map[i] = s;
        result.set_accept(s, a.is_accept(state));
      }
    }

    let mut t = Transition::default();
    for (i, state) in (0..num_states).enumerate() {
      if live_set.contains(i) {
        let num_transitions = a.init_transition(state, &mut t);
        for _ in 0..num_transitions {
          a.get_next_transition(&mut t)?;
          let d = t.dest as usize;
          if live_set.contains(d) {
            result.add_transition(map[i], map[d], t.min, t.max)?;
          }
        }
      }
    }

    result.finish_state()?;
    debug_assert!(!Operations::has_dead_states(&result)?);
    Ok(Cow::Owned(result))
  }
  /// Returns the longest string that is a prefix of all accepted strings and
  /// visits each state at most once. The automaton must not have dead
  /// states.
  ///
  /// Errors:
  /// - Returns an error if the automaton has dead states reachable from the
  ///   initial state.
  ///
  /// Returns:
  /// - The common prefix, which can be an empty (length 0) `String` (never
  ///   `None`).
  pub fn get_common_prefix(a: &Automaton) -> Result<String> {
    let mut prefix = String::new();
    Self::visit_common_prefix(a, |label| {
      prefix.push(label);
      Ok(())
    })?;
    Ok(prefix)
  }

  fn visit_common_prefix(a: &Automaton, mut visit: impl FnMut(char) -> Result<()>) -> Result<()> {
    if Operations::has_dead_states_from_initial(a)? {
      return Err(LuceneError::illegal_argument(
        "input automaton has dead states",
      ));
    }

    if Operations::is_empty(a)? {
      return Ok(());
    }

    let mut scratch = Transition::default();
    let capacity = a.get_num_states() as usize;
    let mut current = FixedBitSet::new(capacity);
    let mut next = FixedBitSet::new(capacity);
    current.set(0)?; // start with initial state
    'algorithm: loop {
      let mut label: i32 = -1;
      let mut state = current.next_set_bit(0);
      // do a pass, stepping all current paths forward once
      while state != NO_MORE_DOCS as usize {
        let state_id = state as i32;
        if a.is_accept(state_id) {
          break 'algorithm;
        }

        for t_idx in 0..a.get_num_transitions_with_state(state_id) {
          a.get_transition(state_id, t_idx, &mut scratch);
          if label == -1 {
            label = scratch.min;
          }
          // either a range of labels, or label that doesn't match all the other paths
          // this round
          if scratch.min != scratch.max || scratch.min != label {
            break 'algorithm;
          }
          next.set(scratch.dest as usize)?;
        }

        if state + 1 >= (current.length()) {
          state = NO_MORE_DOCS as usize
        } else {
          state = current.next_set_bit(state + 1)
        }
      }

      debug_assert!(
        label != -1,
        "we should not get here since we checked no dead-end states up front!?"
      );

      // add this label to prefix
      let label = char::from_u32(label as u32).ok_or_else(|| {
        LuceneError::illegal_state("automaton transition is not a Unicode scalar")
      })?;
      visit(label)?;

      // swap current and next
      std::mem::swap(&mut current, &mut next);
      next.clear()?;
    }

    Ok(())
  }
  /// Returns the longest [`BytesRef`] that is a prefix of all accepted
  /// strings and visits each state at most once.
  ///
  /// Returns:
  /// - The common prefix, which can be an empty (length 0) [`BytesRef`]
  ///   (never `None`), and might possibly include a UTF-8 fragment of a full
  ///   Unicode character.
  pub fn get_common_prefix_bytes_ref(a: &Automaton) -> Result<BytesRef<Vec<u8>>> {
    let mut builder: BytesRefBuilder<Vec<u8>> = BytesRefBuilder::new();
    let mut binary = true;
    Self::visit_common_prefix(a, |label| {
      binary &= label as u32 <= u8::MAX as u32;
      if binary {
        builder.append_byte(label as u8)?;
      }
      Ok(())
    })?;
    if !binary {
      return Err(LuceneError::illegal_state("automaton is not binary"));
    }

    Ok(builder.get_bytes_owner())
  }
  /// If this automaton accepts a single input, returns it. Otherwise, returns
  /// `None`. The automaton must be deterministic.
  pub fn get_singleton(a: &Automaton) -> Result<Option<IntsRef<Vec<i32>>>> {
    if !a.is_deterministic() {
      return Err(LuceneError::illegal_argument(
        "input automaton must be deterministic",
      ));
    }

    let mut builder = IntsRefBuilder::new();
    let mut visited = HashSet::new();
    let mut s = 0;
    let mut t = Transition::default();
    loop {
      visited.insert(s);
      if !a.is_accept(s) {
        if a.get_num_transitions_with_state(s) == 1 {
          a.get_transition(s, 0, &mut t);
          if t.min == t.max && !visited.contains(&t.dest) {
            builder.append(t.min)?;
            s = t.dest;
            continue;
          }
        }
      } else if a.get_num_transitions_with_state(s) == 0 {
        return Ok(Some(builder.get_owner()));
      }

      // Automaton accepts more than one string
      return Ok(None);
    }
  }
  /// Returns the longest [`BytesRef`] that is a suffix of all accepted
  /// strings.
  ///
  /// Worst case complexity: quadratic with the number of states and
  /// transitions.
  ///
  /// Returns:
  /// - The common suffix, which can be an empty (length 0) [`BytesRef`]
  ///   (never `None`).
  pub fn get_common_suffix_bytes_ref(a: &Automaton) -> Result<BytesRef<Vec<u8>>> {
    let v = Operations::reverse(a)?;
    let r = Operations::remove_dead_states(&v)?;
    let mut bytes_ref = Operations::get_common_prefix_bytes_ref(&r)?;
    Operations::reverse_bytes(&mut bytes_ref);
    Ok(bytes_ref)
  }

  fn reverse_bytes(ref_bytes: &mut BytesRef<Vec<u8>>) {
    if ref_bytes.length <= 1 {
      return;
    }
    let bytes = &mut ref_bytes.bytes;
    let offset = ref_bytes.offset;
    let length = ref_bytes.length;
    let num = length / 2;
    let mut i = offset;
    while i < offset + num {
      bytes.swap(i, offset * 2 + length - i - 1);
      i += 1;
    }
  }
  /// Returns an automaton accepting the reverse language.
  pub fn reverse(a: &Automaton) -> Result<Automaton> {
    Operations::reverse_with_initial_states(a, None)
  }
  /// Reverses the automaton, returning the new initial states.
  pub fn reverse_with_initial_states(
    a: &Automaton,
    mut initial_states: Option<&mut BTreeSet<i32>>,
  ) -> Result<Automaton> {
    if Operations::is_empty(a)? {
      return Ok(Automaton::new());
    }

    let num_states = a.get_num_states();
    let num_transitions = a.get_num_transitions();
    // Only state 1 is accepting; reserve transitions without enlarging that bit set.
    let mut builder =
      if num_transitions >= 0 && num_transitions as usize <= ArrayUtil::MAX_ARRAY_LENGTH / 4 {
        Builder::with_capacity(2, num_transitions as usize)
      } else {
        Builder::new()
      };

    builder.create_state(); // New initial state

    for _ in 0..num_states {
      builder.create_state();
    }

    // Old initial state (0) becomes accept state (index 1)
    builder.set_accept(1, true);

    let mut t = Transition::default();
    for s in 0..num_states {
      let num_transitions = a.get_num_transitions_with_state(s);
      a.init_transition(s, &mut t);
      for _ in 0..num_transitions {
        a.get_next_transition(&mut t)?;
        builder.add_transition(t.dest + 1, s + 1, t.min, t.max)?;
      }
    }

    let mut result = builder.finish()?;

    let accept_states = a.get_accept_states();
    let mut s = accept_states.next_set_bit(0);
    while s < num_states && s != -1 {
      result.add_epsilon(0, s + 1)?;
      if let Some(states) = initial_states.as_mut() {
        states.insert(s + 1);
      }
      s += 1;
      s = accept_states.next_set_bit(s as usize);
    }

    result.finish_state()?;

    Ok(result)
  }
  /// Returns a new automaton accepting the same language, with added
  /// transitions to a dead state so that from every state and every label
  /// there is a transition.
  pub fn totalize(a: &Automaton) -> Result<Automaton> {
    let mut result = Automaton::new();

    let num_states = a.get_num_states();
    for i in 0..num_states {
      result.create_state()?;
      result.set_accept(i, a.is_accept(i));
    }

    let dead_state = result.create_state()?;
    result.add_transition(dead_state, dead_state, char::MIN as i32, char::MAX as i32)?;

    let mut t = Transition::default();

    for i in 0..num_states {
      let mut maxi = char::MIN as i32;
      let count = a.init_transition(i, &mut t);

      for _ in 0..count {
        a.get_next_transition(&mut t)?;
        result.add_transition(i, t.dest, t.min, t.max)?;

        if t.min > maxi {
          result.add_transition(i, dead_state, maxi, t.min - 1)?;
        }
        if t.max + 1 > maxi {
          maxi = t.max + 1;
        }
      }

      if maxi <= char::MAX as i32 {
        result.add_transition(i, dead_state, maxi, char::MAX as i32)?;
      }
    }

    result.finish_state()?;
    Ok(result)
  }
  /// Returns the topological sort of all states reachable from the initial
  /// state.
  ///
  /// This method assumes that the automaton does not contain cycles, and will
  /// return an error if a cycle is detected. The CPU cost is
  /// O(num_transitions), and the implementation is non-recursive, so it
  /// will not exhaust the stack for automatons matching long strings.
  /// If there are dead states in the automaton, they will be removed from the
  /// returned array.
  ///
  /// Note: This method uses a deque to iterate the states, which could
  /// potentially consume a lot of heap space for some automatons.
  /// Specifically, automatons with a deep level of states (i.e., a large
  /// number of transitions from the initial state to the final state) may
  /// particularly contribute to high memory usage. The memory consumption
  /// of this method can be considered as O(N), where N is the depth of the
  /// automaton (the maximum number of transitions from the initial state to
  /// any state). However, as this method detects cycles, it will never
  /// attempt to use infinite RAM.
  ///
  /// Parameters:
  /// - `a`: The automaton to be sorted
  ///
  /// Returns:
  /// - The topologically sorted array of state IDs.
  pub fn topo_sort_states(a: &Automaton) -> Result<Vec<i32>> {
    if a.get_num_states() == 0 {
      return Ok(Vec::new());
    }

    let num_states = a.get_num_states();
    let mut states = vec![0; num_states as usize];
    let upto = Operations::topo_sort_states_with_state(a, &mut states)?;

    if upto < states.len() {
      states.truncate(upto);
    }
    states.reverse();
    Ok(states)
  }
  /// Performs a topological sort on the states of the given automaton.
  ///
  /// Parameters:
  /// - `a`: The automaton whose states are to be topologically sorted
  /// - `states`: An array (`&mut [i32]`) which stores the states
  ///
  /// Returns:
  /// - The number of states in the final sorted list.
  ///
  /// Errors:
  /// - Returns an error if the input automaton has a cycle.
  pub fn topo_sort_states_with_state(a: &Automaton, states: &mut [i32]) -> Result<usize> {
    let num_states = a.get_num_states() as usize;
    if num_states != 0 && a.get_num_transitions_with_state(0) == 0 {
      states[0] = 0;
      return Ok(1);
    }
    let mut on_stack = BitSet::with_capacity(num_states);
    let mut visited = BitSet::with_capacity(num_states);
    let mut stack = smallvec::SmallVec::<[i32; 8]>::new();
    stack.push(0); // Assume initial state is 0
    let mut upto = 0;
    let mut t = Transition::default();

    while let Some(&state) = stack.last() {
      let state_index = state as usize;
      let count = a.init_transition(state, &mut t);
      let mut pushed = false;

      for _ in 0..count {
        a.get_next_transition(&mut t)?;
        let dest_index = t.dest as usize;
        if visited.insert(dest_index) {
          stack.push(t.dest);
          on_stack.insert(state_index);
          pushed = true;
          break;
        } else if on_stack.contains(dest_index) {
          return Err(LuceneError::illegal_argument("input automaton has a cycle"));
        }
      }
      // If we haven't pushed any new state onto the stack, we're done with this state
      if !pushed {
        // remove the node from the current recursion stack
        on_stack.remove(state_index);
        stack.pop();
        states[upto] = state;
        upto += 1;
      }
    }
    Ok(upto)
  }
}
#[derive(Default, Clone)]
pub(crate) struct TransitionList {
  // dest, min, max
  pub(crate) transitions: Vec<i32>,
  pub(crate) next: usize,
}

impl TransitionList {
  pub(crate) fn add(&mut self, t: &Transition) -> Result<()> {
    if self.transitions.capacity() < self.next + 3 {
      ArrayUtil::grow_capacity(&mut self.transitions, self.next + 3)?;
    }
    self.transitions.truncate(self.next);
    self.transitions.extend_from_slice(&[t.dest, t.min, t.max]);
    self.next += 3;
    Ok(())
  }
}

#[derive(Default, Clone)]
pub(crate) struct PointTransitions {
  pub(crate) point: i32,
  pub(crate) ends: TransitionList,
  pub(crate) starts: TransitionList,
}

impl PointTransitions {
  pub(crate) fn reset(&mut self, point: i32) {
    self.point = point;
    self.ends.next = 0;
    self.starts.next = 0;
  }
}
impl PartialEq for PointTransitions {
  fn eq(&self, other: &Self) -> bool {
    self.point == other.point
  }
}

impl Eq for PointTransitions {}

impl PartialOrd for PointTransitions {
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    Some(self.cmp(other))
  }
}

impl Ord for PointTransitions {
  fn cmp(&self, other: &Self) -> Ordering {
    self.point.cmp(&other.point)
  }
}

impl std::hash::Hash for PointTransitions {
  fn hash<H>(&self, state: &mut H)
  where
    H: std::hash::Hasher,
  {
    self.point.hash(state);
  }
}

const HASHMAP_CUTOVER: usize = 30;
#[derive(Clone)]
pub(crate) struct PointTransitionSet {
  pub(crate) count: usize,
  pub(crate) points: Vec<PointTransitions>,
  map: HashMap<i32, usize>,
  use_hash: bool,
}

impl PointTransitionSet {
  pub(crate) fn new() -> Self {
    PointTransitionSet {
      count: 0,
      points: Vec::with_capacity(5),
      map: HashMap::new(),
      use_hash: false,
    }
  }
  fn next(&mut self, point: i32) -> Result<usize> {
    if self.count == self.points.len() {
      ArrayUtil::grow_capacity(&mut self.points, 1 + self.count)?;
      self.points.push(PointTransitions {
        point,
        ends: TransitionList {
          transitions: Vec::new(),
          next: 0,
        },
        starts: TransitionList {
          transitions: Vec::new(),
          next: 0,
        },
      });
    }
    let points0 = &mut self.points[self.count];
    points0.reset(point);
    self.count += 1;
    Ok(self.count - 1)
  }
  pub fn find(&mut self, point: i32) -> Result<&mut PointTransitions> {
    if self.use_hash {
      if let Some(index) = self.map.get(&point).copied() {
        return Ok(&mut self.points[index]);
      }
      let p = self.next(point)?;
      self.map.insert(point, p);
      Ok(&mut self.points[p])
    } else {
      for i in 0..self.count {
        if self.points[i].point == point {
          return Ok(&mut self.points[i]);
        }
      }

      let p = self.next(point)?;
      if self.count == HASHMAP_CUTOVER {
        debug_assert!(self.map.is_empty());
        self.map.reserve(self.count);
        for i in 0..self.count {
          self.map.insert(self.points[i].point, i);
        }
        self.use_hash = true;
      }
      Ok(&mut self.points[p])
    }
  }

  pub fn reset(&mut self) {
    if self.use_hash {
      self.map.clear();
      self.use_hash = false;
    }
    self.count = 0;
  }

  pub fn sort(&mut self) -> Result<()> {
    if self.count > 1 {
      self.points[0..self.count].sort_unstable_by_key(|p| p.point);
    }
    Ok(())
  }
  pub fn add(&mut self, t: &Transition) -> Result<()> {
    self.find(t.min)?.starts.add(t)?;
    self.find(t.max + 1)?.ends.add(t)
  }
}
impl Display for PointTransitionSet {
  fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
    for i in 0..self.count {
      if i > 0 {
        write!(f, " ")?;
      }
      let pt = &self.points[i];
      write!(
        f,
        "{}:{},{}",
        pt.point,
        pt.starts.next / 3,
        pt.ends.next / 3
      )?;
    }
    Ok(())
  }
}
