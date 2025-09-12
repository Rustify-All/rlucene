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
/// If the [`Automaton`] has cycles, then this iterator may throw an error,
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
    pub fn new(a: &'a Automaton) -> Self {
        Self::with_start_end(a, 0, -1)
    }

    /// Constructs an iterator for all finite strings of the automaton starting
    /// from given state
    pub fn with_start_end(a: &'a Automaton, start_state: i32, end_state: i32) -> Self {
        let num_states = a.get_num_states();
        let mut nodes = Vec::with_capacity(16);
        for _ in 0..16 {
            nodes.push(PathNode::new());
        }

        let mut path_states = BitSet::with_capacity(num_states as usize);
        let mut string = IntsRefBuilder::new();

        let emit_empty_string = a.is_accept(start_state);

        if num_states > start_state && a.get_num_transitions_with_state(start_state) > 0 {
            path_states.insert(start_state as usize);
            nodes[0].reset_state(a, start_state);
            string.append(start_state);
        }

        Self {
            a,
            end_state,
            path_states,
            string,
            nodes,
            emit_empty_string,
        }
    }

    /// Grow path stack, if required.
    fn grow_stack(&mut self, depth: usize) -> Result<()> {
        if self.nodes.len() == depth {
            let min_target_size = self.nodes.len() + 1;
            // TODO: _bytes_per_element `4` currently is a padding value
            let new_len = ArrayUtil::oversize(min_target_size, 4);
            ArrayUtil::grow_exact(&mut self.nodes, new_len)?;
        }
        Ok(())
    }
}
impl FiniteStringsIteratorBase for FiniteStringsIterator<'_> {
    fn next(&mut self) -> Result<Option<Cow<'_, IntsRef<Vec<i32>>>>> {
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
                    self.string.grow(depth);
                } else if self.end_state == to || self.a.is_accept(to) {
                    // This transition leads to an accept state, so we save the current string:
                    return Ok(Some(Cow::Borrowed(self.string.get())));
                }
            } else {
                // No more transitions leaving this state, pop/return back to previous state:
                let state = node.state;
                assert!(self.path_states.contains(state as usize));
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
#[cfg(test)]
pub(crate) mod tests {
    use std::borrow::Cow;
    use std::collections::HashSet;

    use rand::Rng;

    use crate::core::index::BytesRef;
    use crate::core::util::automation::automata::Automata;
    use crate::core::util::automation::automaton::Automaton;
    use crate::core::util::automation::finite_strings_iterator::{
        FiniteStringsIterator, FiniteStringsIteratorBase,
    };
    use crate::core::util::automation::operations::Operations;
    use crate::core::util::automation::operations::tests::TestOperations;
    use crate::core::util::automation::reg_exp::RegExp;
    use crate::core::util::error::lucene_error::{LuceneError, Result};
    use crate::core::util::fst_impl::util::Util;
    use crate::core::util::ints_ref::IntsRef;
    use crate::core::util::ints_ref_builder::IntsRefBuilder;
    use crate::test::util::automaton::automaton_test_util::AutomatonTestUtil;
    use crate::test::util::automaton::minimization_operation::MinimizationOperations;
    use crate::test::util::lucene_test_case::lucene_test_case_util::random;
    use crate::test::util::test_util::TestUtil;
    /// Test for FiniteStringsIterator.
    #[allow(dead_code)] // for quick search
    struct TestFiniteStringsIterator;
    #[test]
    fn test_random_finite_strings1() -> Result<()> {
        let mut random = random();
        // let num_strings = at_least(&mut random, 100);
        let num_strings = 1;
        if cfg!(feature = "test_log_verbose") {
            println!("TEST: num_strings={}", num_strings);
        }

        let mut strings = HashSet::new();
        let mut string_list = Vec::new();
        let mut scratch = IntsRefBuilder::new();

        for _ in 0..num_strings {
            let s = TestUtil::random_simple_string_range(&mut random, 1, 200);
            Util::get_utf32_with_slice(&s, 0, s.len(), &mut scratch);
            if strings.insert(scratch.to_ints_ref()) {
                string_list.push(Automata::make_string(&s)?);
                if cfg!(feature = "test_log_verbose") {
                    println!("  add string={}", s);
                }
            }
        }
        let refs: Vec<&Automaton> = string_list.iter().collect();
        let a = Operations::union_list(&refs)?;

        let a = if random.random_bool(0.5) {
            let v = MinimizationOperations::minimize(&a, 1_000_000)?;
            if cfg!(feature = "test_log_verbose") {
                println!("TEST: a.minimize numStates={}", a.get_num_states());
            }
            v
        } else if random.random_bool(0.5) {
            if cfg!(feature = "test_log_verbose") {
                println!("TEST: a.determinize");
            }
            Operations::determinize(&a, 1_000_000)?
        } else if random.random_bool(0.5) {
            if cfg!(feature = "test_log_verbose") {
                println!("TEST: a.removeDeadStates");
            }
            Operations::remove_dead_states(&a)?
        } else {
            Cow::Owned(a)
        };

        let mut iterator = FiniteStringsIterator::new(&a);
        let actual = get_finite_strings(&mut iterator)?;
        assert_finite_strings_recursive(&a, actual.clone());

        let actual_set: HashSet<_> = actual.into_iter().collect();

        if strings != actual_set {
            if cfg!(feature = "test_log_verbose") {
                println!(
                    "strings.size()={} actual.size={}",
                    strings.len(),
                    actual_set.len()
                );
            }

            let mut x: Vec<_> = strings.into_iter().collect();
            let mut y: Vec<_> = actual_set.into_iter().collect();
            x.sort();
            y.sort();

            let end = x.len().min(y.len());
            for i in 0..end {
                if cfg!(feature = "test_log_verbose") {
                    println!(
                        "  i={} string={} actual={}",
                        i,
                        to_ascii_string(&x[i]),
                        to_ascii_string(&y[i])
                    );
                }
            }
            unreachable!("wrong strings found");
        }

        Ok(())
    }

    /// Basic test for getFiniteStrings
    #[test]
    fn test_finite_strings_basic() -> Result<()> {
        let a = Operations::union(
            &Automata::make_string("dog")?,
            &Automata::make_string("duck")?,
        )?;
        let a = MinimizationOperations::minimize(&a, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;
        let mut iterator = FiniteStringsIterator::new(&a);
        let actual = get_finite_strings(&mut iterator)?;

        assert_finite_strings_recursive(&a, actual.clone());
        assert_eq!(actual.len(), 2);

        let mut dog = IntsRefBuilder::new();
        Util::get_ints_ref(&BytesRef::<Vec<u8>>::from_string("dog"), &mut dog);
        assert!(actual.contains(dog.get()));

        let mut duck = IntsRefBuilder::new();
        Util::get_ints_ref(&BytesRef::<Vec<u8>>::from_string("duck"), &mut duck);
        assert!(actual.contains(duck.get()));

        Ok(())
    }

    #[test]
    fn test_finite_strings_eats_stack() -> Result<()> {
        let mut chars = vec![0u16; 50000];
        let mut random = random();
        let chars_len = chars.len();
        TestUtil::random_fixed_length_unicode_string_with_chars(
            &mut random,
            &mut chars,
            0,
            chars_len,
        );
        let big_string1 = String::from_utf16(&chars).unwrap();
        TestUtil::random_fixed_length_unicode_string_with_chars(
            &mut random,
            &mut chars,
            0,
            chars_len,
        );
        let big_string2 = String::from_utf16(&chars).unwrap();

        let a = Operations::union(
            &Automata::make_string(&big_string1)?,
            &Automata::make_string(&big_string2)?,
        )?;

        let mut iterator = FiniteStringsIterator::new(&a);
        let actual = get_finite_strings(&mut iterator)?;
        assert_eq!(actual.len(), 2);

        let mut scratch = IntsRefBuilder::new();
        Util::get_utf32_with_slice(&big_string1, 0, big_string1.len(), &mut scratch);
        assert!(actual.contains(scratch.get()));

        Util::get_utf32_with_slice(&big_string2, 0, big_string2.len(), &mut scratch);
        assert!(actual.contains(scratch.get()));

        Ok(())
    }

    #[test]
    fn test_with_cycle() {
        let result = (|| {
            let a = RegExp::from_str_with_flags("abc.*", RegExp::NONE)?.to_automaton()?;
            let mut iterator = FiniteStringsIterator::new(&a);
            get_finite_strings(&mut iterator)?;
            Ok::<(), LuceneError>(())
        })();
        assert!(matches!(result, Err(LuceneError::IllegalArgument(_))));
    }

    #[test]
    fn test_singleton_no_limit() -> Result<()> {
        let a = Automata::make_string("foobar")?;
        let mut iterator = FiniteStringsIterator::new(&a);
        let actual = get_finite_strings(&mut iterator)?;
        assert_eq!(actual.len(), 1);

        let mut scratch = IntsRefBuilder::new();
        Util::get_utf32_with_slice("foobar", 0, 6, &mut scratch);
        assert!(actual.contains(scratch.get()));

        Ok(())
    }

    #[test]
    fn test_short_accept() -> Result<()> {
        let a = Operations::union(&Automata::make_string("x")?, &Automata::make_string("xy")?)?;
        let a = MinimizationOperations::minimize(&a, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;

        let mut iterator = FiniteStringsIterator::new(&a);
        let actual = get_finite_strings(&mut iterator)?;
        assert_eq!(actual.len(), 2);

        let mut x = IntsRefBuilder::new();
        Util::get_ints_ref(&BytesRef::<Vec<u8>>::from_string("x"), &mut x);
        assert!(actual.contains(x.get()));

        let mut xy = IntsRefBuilder::new();
        Util::get_ints_ref(&BytesRef::<Vec<u8>>::from_string("xy"), &mut xy);
        assert!(actual.contains(xy.get()));
        Ok(())
    }

    #[test]
    fn test_single_string() -> Result<()> {
        let mut a = Automaton::new();
        let start = a.create_state();
        let end = a.create_state();
        a.set_accept(end, true);
        a.add_transition(start, end, 'a' as i32, 'a' as i32)?;
        a.finish_state()?;

        let accepted = TestOperations::get_finite_strings(&a)?;

        assert_eq!(accepted.len(), 1);

        let mut ints_ref = IntsRefBuilder::new();
        ints_ref.append('a' as i32);

        assert!(accepted.contains(&ints_ref.to_ints_ref()));
        Ok(())
    }

    /// All strings generated by the iterator.
    pub(crate) fn get_finite_strings(
        iterator: &mut impl FiniteStringsIteratorBase,
    ) -> Result<Vec<IntsRef<Vec<i32>>>> {
        let mut result = Vec::new();
        while let Some(finite_string) = iterator.next()? {
            result.push(IntsRef::deep_copy_of(&finite_string));
        }
        Ok(result)
    }

    /// Checks that the strings returned by the automaton are as expected.
    ///
    /// Parameters:
    /// - `automaton`: The automaton
    /// - `actual`: Strings generated by the automaton
    fn assert_finite_strings_recursive(automaton: &Automaton, actual: Vec<IntsRef<Vec<i32>>>) {
        let expected = AutomatonTestUtil::get_finite_strings_recursive(automaton, -1);

        // Check that no string is emitted twice
        assert_eq!(
            expected.len(),
            actual.len(),
            "Expected and actual lengths differ"
        );

        let actual_set: HashSet<_> = actual.into_iter().collect();
        assert_eq!(expected, actual_set, "Expected and actual sets differ");
    }

    /// Only handles ASCII (for this test helper).
    fn to_ascii_string(ints: &IntsRef<Vec<i32>>) -> String {
        let mut bytes = Vec::with_capacity(ints.length);
        for i in 0..ints.length {
            bytes.push(ints.ints[ints.offset + i] as u8);
        }
        String::from_utf8(bytes).expect("Only ASCII supported in intsref_to_ascii_string")
    }
}
