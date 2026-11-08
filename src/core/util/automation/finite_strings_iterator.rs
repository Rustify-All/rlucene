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

use bit_set::BitSet;

use crate::core::util::array_util::ArrayUtil;
use crate::core::util::automation::automaton::Automaton;
use crate::core::util::automation::transition::Transition;
use crate::core::util::automation::transition_accessor::TransitionAccessor;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::ints_ref::IntsRef;
use crate::core::util::ints_ref_builder::IntsRefBuilder;

/// Iterates all accepted strings.
///
/// If the [`Automaton`] has cycles, then this iterator may return an error,
/// but this is not guaranteed.
///
/// Be aware that the iteration order is implementation dependent and may change
/// across releases.
///
/// If the automaton is not determinized, then it is possible this iterator will
/// return duplicates.
#[derive(Debug)]
pub struct FiniteStringsIterator<'a> {
  /// Automaton to create finite string from.
  a: &'a Automaton,
  /// The state where each path should stop or -1 if only accepted states
  /// should be final.
  end_state: i32,
  /// Tracks which states are in the current path, for cycle detection.
  path_states: BitSet,
  /// Builder for current finite string.
  string: IntsRefBuilder<Vec<i32>>,
  /// Stack to hold our current state in the recursion/iteration.
  nodes: Vec<PathNode>,
  /// Emit empty string?
  emit_empty_string: bool,
}

impl<'a> FiniteStringsIterator<'a> {
  /// Constructs an iterator for all finite strings of the automaton starting
  /// from 0
  pub fn new(a: &'a Automaton) -> Result<Self> {
    Self::with_start_end(a, 0, -1)
  }

  /// Constructs an iterator for all finite strings of the automaton starting
  /// from given state
  pub fn with_start_end(a: &'a Automaton, start_state: i32, end_state: i32) -> Result<Self> {
    let num_states = a.get_num_states();
    let mut nodes = Vec::new();
    let mut path_states = BitSet::new();
    let mut string = IntsRefBuilder::new();

    let emit_empty_string = a.is_accept(start_state);

    if num_states > start_state && a.get_num_transitions_with_state(start_state) > 0 {
      nodes.reserve_exact((num_states as usize).clamp(1, 16));
      let mut node = PathNode::new();
      node.reset_state(a, start_state);
      nodes.push(node);
      path_states = BitSet::with_capacity(num_states as usize);
      path_states.insert(start_state as usize);
      string.append(start_state)?;
    }

    Ok(Self {
      a,
      end_state,
      path_states,
      string,
      nodes,
      emit_empty_string,
    })
  }

  /// Grow path stack, if required.
  fn grow_stack(&mut self, depth: usize) -> Result<()> {
    if self.nodes.len() == depth {
      let min_target_size = self.nodes.len() + 1;
      ArrayUtil::grow_capacity(&mut self.nodes, min_target_size)?;
      self.nodes.push(PathNode::new());
    }
    Ok(())
  }

  /// Generates the next finite string.
  ///
  /// The return value is only valid until the next call of this method.
  #[allow(clippy::should_implement_trait)]
  // std::Iterator cannot return the reused buffer borrowed from self.
  pub fn next(&mut self) -> Result<Option<Cow<'_, IntsRef<Vec<i32>>>>> {
    // Special case the empty string, as usual:
    if self.emit_empty_string {
      self.emit_empty_string = false;
      return Ok(Some(Cow::Owned(IntsRef::new())));
    }

    let mut depth = self.string.length();

    while depth > 0 {
      let node = &mut self.nodes[depth - 1];

      // Get next label leaving current node
      let label = node.next_label(self.a);
      if label != -1 {
        self.string.set_int_at(depth - 1, label);

        let to = node.to;
        if self.a.get_num_transitions_with_state(to) != 0 && to != self.end_state {
          // Now recurse: the destination of this transition has outgoing transitions:
          if self.path_states.contains(to as usize) {
            return Err(LuceneError::illegal_argument("automaton has cycles"));
          }
          self.path_states.insert(to as usize);
          // Push node onto stack:
          self.grow_stack(depth)?;
          self.nodes[depth].reset_state(self.a, to);
          depth += 1;
          self.string.set_length(depth);
          self.string.grow(depth)?;
        } else if self.end_state == to || self.a.is_accept(to) {
          // This transition leads to an accept state, so we save the current string:
          return Ok(Some(Cow::Borrowed(self.string.get())));
        }
      } else {
        // No more transitions leaving this state, pop/return back to previous state:
        let state = node.state;
        debug_assert!(self.path_states.contains(state as usize));
        self.path_states.remove(state as usize);

        depth -= 1;
        self.string.set_length(depth);

        if self.a.is_accept(state) {
          // This transition leads to an accept state, so we save the current string:
          return Ok(Some(Cow::Borrowed(self.string.get())));
        }
      }
      depth = self.string.length();
    }
    // Finished iteration.
    Ok(None)
  }
}

#[cfg(test)]
impl FiniteStringsIteratorBase for FiniteStringsIterator<'_> {
  fn next(&mut self) -> Result<Option<Cow<'_, IntsRef<Vec<i32>>>>> {
    FiniteStringsIterator::next(self)
  }
}

#[cfg(test)]
pub(crate) trait FiniteStringsIteratorBase {
  /// Generates the next finite string.
  ///
  /// The return value is only valid until the next call of this method!
  ///
  /// Returns:
  /// - The next finite string, or `None` if no more finite strings are
  ///   available.
  fn next(&mut self) -> Result<Option<Cow<'_, IntsRef<Vec<i32>>>>>;
}

#[derive(Debug)]
pub(crate) struct PathNode {
  /// Which state the path node ends on, whose transitions we are enumerating.
  pub(crate) state: i32,
  /// Which state the current transition leads to.
  pub(crate) to: i32,
  /// Which transition we are on.
  pub(crate) transition: i32,
  /// Which label we are on, in the min-max range of the current Transition
  pub(crate) label: i32,
  t: Transition,
}
impl Default for PathNode {
  fn default() -> Self {
    PathNode::new()
  }
}
impl PathNode {
  pub fn new() -> Self {
    Self {
      state: 0,
      to: 0,
      transition: 0,
      label: 0,
      t: Transition::default(),
    }
  }

  /// Resets this node to start enumerating transitions leaving given state.
  pub fn reset_state(&mut self, a: &Automaton, state: i32) {
    debug_assert!(a.get_num_transitions_with_state(state) != 0);
    self.state = state;
    self.transition = 0;
    a.get_transition(state, 0, &mut self.t);
    self.label = self.t.min;
    self.to = self.t.dest;
  }

  /// Returns next label of current transition, or advances to next
  /// transition. If there are no more transitions, returns -1.
  pub fn next_label(&mut self, a: &Automaton) -> i32 {
    if self.label > self.t.max {
      // We've exhaused the current transition's labels;
      // move to next transitions:
      self.transition += 1;
      if self.transition >= a.get_num_transitions_with_state(self.state) {
        // We're done iterating transitions leaving this state
        self.label = -1;
        return -1;
      }
      a.get_transition(self.state, self.transition, &mut self.t);
      self.label = self.t.min;
      self.to = self.t.dest;
    }
    let ret = self.label;
    self.label += 1;
    ret
  }
}
