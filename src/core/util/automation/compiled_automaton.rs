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
use crate::core::index::{BytesRef, BytesRefBuilder};
use crate::core::util::StringHelper;
use crate::core::util::automation::automaton::Automaton;
use crate::core::util::automation::byte_run_automaton::ByteRunAutomaton;
use crate::core::util::automation::byte_runnable::{ByteRunnable, ByteRunnableEnum};
use crate::core::util::automation::nfa_run_automaton::NFARunAutomaton;
use crate::core::util::automation::operations::Operations;
use crate::core::util::automation::transition::Transition;
use crate::core::util::automation::transition_accessor::{
    TransitionAccessor, TransitionAccessorEnum,
};
use crate::core::util::automation::utf32_to_utf8::UTF32ToUTF8;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::unicode_util::UnicodeUtil;
use std::borrow::Cow;
use std::rc::Rc;

/// Automata are compiled into different internal forms for the most efficient
/// execution depending upon the language they accept.
pub struct CompiledAutomaton {
    /// If `simplify` is true this will be the "simplified" type; else, this is
    /// NORMAL
    pub automaton_type: AutomatonType,

    /// For [`AutomatonType::Single`] this is the singleton term.
    pub term: Option<BytesRef<Vec<u8>>>,

    /// Matcher for quickly determining if a byte[] is accepted. Only valid for
    /// [`AutomatonType::Normal`].
    pub run_automaton: Option<Rc<ByteRunAutomaton>>,

    /// Matcher directly run on a NFA, it will determinize the state on need and
    /// caches it, note that this field and
    /// [`CompiledAutomaton::run_automaton`] will not be non-null at the same
    /// time.
    ///
    /// TODO: merge this with run_automaton
    nfa_run_automaton: Option<Rc<NFARunAutomaton>>,

    /// Shared common suffix accepted by the automaton. Only valid for
    /// [`AutomatonType::Normal`], and only when the automaton accepts an
    /// infinite language. This will be `None` if the common prefix is
    /// length 0.
    pub common_suffix_ref: Option<Rc<BytesRef<Vec<u8>>>>,

    /// Indicates if the automaton accepts a finite set of strings.
    /// Only valid for [`AutomatonType::Normal`].
    pub finite: bool,

    /// Which state, if any, accepts all suffixes, else -1.
    pub sink_state: i32,

    transition: Transition,
}
impl CompiledAutomaton {
    /// Creates a `CompiledAutomaton` with `finite = false` and `simplify =
    /// true`.
    pub fn from_automaton(automaton: Automaton) -> Result<Self> {
        Self::with_binary(automaton, false, true, false)
    }
    /// Returns sink state, if present, else -1.
    fn find_sink_state(automaton: &Automaton) -> i32 {
        let num_states = automaton.get_num_states();
        let mut t = Transition::default();
        for s in 0..num_states {
            if automaton.is_accept(s) {
                let count = automaton.init_transition(s, &mut t);
                for _ in 0..count {
                    automaton.get_next_transition(&mut t);
                    if t.dest == s && t.min == 0 && t.max == 0xff {
                        return s;
                    }
                }
            }
        }
        -1
    }
    /// Create this. If `simplify` is true, we run possibly expensive operations
    /// to determine if the automaton is one of the cases in
    /// [`AutomatonType`]. Set `finite` to `true` if the automaton is
    /// finite, otherwise set to `false` if infinite or unknown.
    pub fn new(automaton: Automaton, finite: bool, simplify: bool) -> Result<Self> {
        Self::with_binary(automaton, finite, simplify, false)
    }
    /// Creates a new instance.  
    ///
    /// If `simplify` is true, possibly expensive operations will be performed
    /// to determine if the automaton is one of the cases in
    /// [`AutomatonType`]. Set `finite` to `true` if the automaton is
    /// finite, or `false` if it is infinite or unknown.
    pub fn with_binary(
        mut automaton: Automaton,
        finite: bool,
        simplify: bool,
        is_binary: bool,
    ) -> Result<Self> {
        if automaton.get_num_states() == 0 {
            automaton = Automaton::new();
            automaton.create_state();
        }
        // simplify requires a DFA
        if simplify && automaton.is_deterministic() {
            // Test whether the automaton is a "simple" form and
            // if so, don't create a runAutomaton.  Note that on a
            // large automaton these tests could be costly:
            if Operations::is_empty(&automaton) {
                return Ok(Self {
                    automaton_type: AutomatonType::None,
                    term: None,
                    run_automaton: None,

                    nfa_run_automaton: None,
                    common_suffix_ref: None,
                    finite: true,
                    sink_state: -1,
                    transition: Transition::default(),
                });
            }
            // NOTE: only approximate, because automaton may not be minimal:
            let is_total = if is_binary {
                Operations::is_total_with_range(&automaton, 0, 0xff)?
            } else {
                Operations::is_total(&automaton)?
            };

            if is_total {
                // matches all possible strings
                return Ok(Self {
                    automaton_type: AutomatonType::All,
                    term: None,
                    run_automaton: None,

                    nfa_run_automaton: None,
                    common_suffix_ref: None,
                    finite: false,
                    sink_state: -1,
                    transition: Transition::default(),
                });
            }

            if let Some(singleton) = Operations::get_singleton(&automaton)? {
                let term = if is_binary {
                    Some(StringHelper::ints_ref_to_bytes_ref(&singleton)?)
                } else {
                    Some(BytesRef::from_string(&UnicodeUtil::new_string(
                        singleton.ints.as_slice(),
                        singleton.offset,
                        singleton.length,
                    )?))
                };

                return Ok(Self {
                    automaton_type: AutomatonType::Single,
                    term,
                    run_automaton: None,

                    nfa_run_automaton: None,
                    common_suffix_ref: None,
                    finite: true,
                    sink_state: -1,
                    transition: Transition::default(),
                });
            }
        }

        let automaton_type = AutomatonType::Normal;
        let term = None;

        let automaton_is_deterministic = automaton.is_deterministic();
        let binary = if is_binary {
            // Caller already built binary automaton themselves, e.g. PrefixQuery
            // does this since it can be provided with a binary (not necessarily
            // UTF8!) term:
            automaton
        } else {
            // Incoming automaton is unicode, and we must convert to UTF8 to match what's in
            // the index:
            match UTF32ToUTF8::new().convert(&automaton)? {
                Cow::Borrowed(_) => automaton,
                Cow::Owned(o) => o,
            }
        };
        // compute a common suffix for infinite DFAs, this is an optimization for
        // "leading wildcard" so don't burn cycles on it if the DFA is finite,
        // or largeish
        let common_suffix_ref =
            if finite || binary.get_num_states() + binary.get_num_transitions() > 1000 {
                None
            } else {
                let suffix = Operations::get_common_suffix_bytes_ref(&binary)?;
                if suffix.length == 0 {
                    None
                } else {
                    Some(Rc::new(suffix))
                }
            };

        if !automaton_is_deterministic && !binary.is_deterministic() {
            Ok(Self {
                automaton_type,
                term,
                run_automaton: None,

                nfa_run_automaton: Some(Rc::new(NFARunAutomaton::with_alphabet_size(binary, 0xff))),
                common_suffix_ref,
                finite,
                sink_state: -1,
                transition: Transition::default(),
            })
        } else {
            // We already had a DFA (or threw exception), according to mike UTF32toUTF8
            // won't "blow up"
            let dfa = match Operations::determinize(&binary, i32::MAX as usize)? {
                Cow::Borrowed(_) => binary,
                Cow::Owned(o) => o,
            };
            let run_automaton = ByteRunAutomaton::with_bool(dfa, true)?;
            let sink_state = Self::find_sink_state(&run_automaton.base.automaton);

            Ok(Self {
                automaton_type,
                term,
                run_automaton: Some(Rc::new(run_automaton)),
                nfa_run_automaton: None,
                common_suffix_ref,
                finite,
                sink_state,
                transition: Transition::default(),
            })
        }
    }
    fn add_tail(
        &mut self,
        mut state: i32,
        term: &mut BytesRefBuilder<Vec<u8>>,
        mut idx: usize,
        lead_label: i32,
    ) -> Result<BytesRef<Vec<u8>>> {
        let mut max_index = -1;
        let automaton = &self.run_automaton.as_ref().unwrap().base.automaton;
        let num_transitions = automaton.init_transition(state, &mut self.transition);
        for i in 0..num_transitions {
            automaton.get_next_transition(&mut self.transition);
            if self.transition.min < lead_label {
                max_index = i;
            } else {
                // Transitions are always sorted
                break;
            }
        }

        debug_assert!(max_index != -1);
        automaton.get_transition(state, max_index, &mut self.transition);
        // Append floorLabel
        let floor_label = if self.transition.max > lead_label - 1 {
            lead_label - 1
        } else {
            self.transition.max
        };

        term.grow(idx + 1);
        term.set_byte_at(idx, floor_label as u8);
        state = self.transition.dest;
        idx += 1;

        loop {
            let num_transitions = automaton.get_num_transitions_with_state(state);

            if num_transitions == 0 {
                debug_assert!(self.run_automaton.as_ref().unwrap().is_accept(state)?);
                term.set_length(idx);
                return Ok(term.get_bytes_owner());
            }

            automaton.get_transition(state, num_transitions - 1, &mut self.transition);
            term.grow(idx + 1);
            term.set_byte_at(idx, self.transition.max as u8);
            state = self.transition.dest;
            idx += 1;
        }
    }
    /// Finds the largest term accepted by this [`Automaton`] that is `<=` the
    /// provided input term.
    ///
    /// The result is placed in `output`; it is fine for `output` and `input` to
    /// point to the same bytes. The returned result is either the provided
    /// `output`, or `None` if there is no floor term (i.e., the input term
    /// is before the first term accepted by this automaton).
    pub fn floor(
        &mut self,
        input: &BytesRef<Vec<u8>>,
        output: &mut BytesRefBuilder<Vec<u8>>,
    ) -> Result<Option<BytesRef<Vec<u8>>>> {
        let run_automaton = self.run_automaton.as_ref().unwrap();
        let automaton = &run_automaton.base.automaton;
        let mut state = 0;

        // Special case: empty string
        if input.length == 0 {
            return if run_automaton.is_accept(state)? {
                output.clear();
                Ok(Some(output.get_bytes_owner()))
            } else {
                Ok(None)
            };
        }

        let mut idx = 0;
        let mut stack = Vec::with_capacity(input.length);

        loop {
            let mut label = input.bytes[input.offset + idx] as i32;
            let mut next_state = run_automaton.step(state, label);

            if idx == input.length - 1 {
                if next_state != -1 && run_automaton.is_accept(next_state)? {
                    output.grow(idx + 1);
                    output.set_byte_at(idx, label as u8);
                    output.set_length(input.length);
                    return Ok(Some(output.get_bytes_owner()));
                } else {
                    next_state = -1;
                }
            }

            if next_state == -1 {
                // Pop back to a state that has a transition <= our label:
                loop {
                    let num_transitions = automaton.get_num_transitions_with_state(state);
                    if num_transitions == 0 {
                        debug_assert!(run_automaton.is_accept(state)?);
                        output.set_length(idx);
                        return Ok(Some(output.get_bytes_owner()));
                    } else {
                        automaton.get_transition(state, 0, &mut self.transition);
                        if label - 1 < self.transition.min {
                            if run_automaton.is_accept(state)? {
                                output.set_length(idx);
                                return Ok(Some(output.get_bytes_owner()));
                            }
                            if stack.is_empty() {
                                return Ok(None);
                            } else {
                                state = stack.pop().unwrap();
                                idx -= 1;
                                label = input.bytes[input.offset + idx] as i32;
                            }
                        } else {
                            break;
                        }
                    }
                }
                return Ok(Some(self.add_tail(state, output, idx, label)?));
            } else {
                output.grow(idx + 1);
                output.set_byte_at(idx, label as u8);
                stack.push(state);
                state = next_state;
                idx += 1;
            }
        }
    }
    /// Returns a [`ByteRunnable`] instance, which differs depending on whether
    /// an NFA or DFA is passed in. This method does not guarantee returning
    /// a non-null object.
    pub fn get_byte_runnable(&mut self) -> ByteRunnableEnum {
        debug_assert!(self.nfa_run_automaton.is_none() || self.run_automaton.is_none());

        if self.nfa_run_automaton.is_some() {
            ByteRunnableEnum::NFA(self.nfa_run_automaton.as_ref().unwrap().clone())
        } else {
            ByteRunnableEnum::Byte(self.run_automaton.as_ref().unwrap().clone())
        }
    }
    /// Returns a [`TransitionAccessor`] instance, which varies depending on
    /// whether an NFA or DFA is passed in. This method does not guarantee
    /// returning a non-null object.
    pub fn get_transition_accessor(&mut self) -> TransitionAccessorEnum {
        debug_assert!(self.nfa_run_automaton.is_none() || self.run_automaton.is_some());

        if self.nfa_run_automaton.is_some() {
            TransitionAccessorEnum::Nfa(self.nfa_run_automaton.as_ref().unwrap().clone())
        } else {
            TransitionAccessorEnum::Byte(self.run_automaton.as_ref().unwrap().clone())
        }
    }
}

/// Automata are compiled into different internal forms for the most efficient
/// execution depending upon the language they accept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomatonType {
    /// Automaton that accepts no strings.
    None,
    /// Automaton that accepts all possible strings.
    All,
    /// Automaton that accepts only a single fixed string.
    Single,
    /// Catch-all for any other automata.
    Normal,
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use rand::Rng;

    use crate::core::index::{BytesRef, BytesRefBuilder};
    use crate::core::util::automation::automata::Automata;
    use crate::core::util::automation::automaton::Automaton;
    use crate::core::util::automation::compiled_automaton::{AutomatonType, CompiledAutomaton};
    use crate::core::util::automation::operations::Operations;
    use crate::core::util::error::lucene_error::Result;
    use crate::test::util::lucene_test_case::lucene_test_case_util::{
        at_least, random, random_multiplier,
    };
    use crate::test::util::test_util::TestUtil;
    #[allow(dead_code)] // for quick search
    struct TestCompiledAutomaton;
    fn build(_determinize_work_limit: i32, strings: &[&str]) -> Result<CompiledAutomaton> {
        let mut terms: Vec<BytesRef<Vec<u8>>> =
            strings.iter().map(|s| BytesRef::from_string(s)).collect();

        terms.sort();
        let a = Automata::make_string_union(&terms)?;
        CompiledAutomaton::with_binary(a, true, false, false)
    }
    fn test_floor(c: &mut CompiledAutomaton, input: &str, expected: Option<&str>) -> Result<()> {
        let b = BytesRef::from_string(input);
        let mut builder = BytesRefBuilder::default();

        let result = c.floor(&b, &mut builder)?;

        match expected {
            None => {
                assert!(result.is_none(), "Expected None, got {:?}", result);
            },
            Some(expected_str) => {
                let result = result.expect("Expected Some(BytesRef), got None");
                let expected_bytes = BytesRef::from_string(expected_str);
                assert_eq!(
                    result, expected_bytes,
                    "actual={:?} vs expected={} (input={})",
                    result, expected_str, input
                );
            },
        }

        Ok(())
    }
    fn test_terms<R: Rng + ?Sized>(
        random: &mut R,
        determinize_work_limit: i32,
        terms: &[&str],
    ) -> Result<()> {
        let mut compiled = build(determinize_work_limit, terms)?;
        let mut term_bytes: Vec<BytesRef<Vec<u8>>> =
            terms.iter().map(|s| BytesRef::from_string(s)).collect();
        term_bytes.sort();

        if cfg!(feature = "test_log_verbose") {
            println!("\nTEST: terms in unicode order");
            for t in &term_bytes {
                println!("  {}", t.utf8_to_string()?);
            }
        }

        for _ in 0..(100 * random_multiplier() as usize) {
            let s = if random.random_range(0..10) == 1 {
                terms[random.random_range(0..terms.len())].to_string()
            } else {
                random_string(random)
            };

            if cfg!(feature = "test_log_verbose") {
                println!("\nTEST: floor({s})");
            }

            let key = BytesRef::from_string(&s);
            let mut expected: Option<String> = None;

            match term_bytes.binary_search(&key) {
                Ok(_) => {
                    expected = Some(s.clone());
                },
                Err(insert_pos) => {
                    if insert_pos > 0 {
                        expected = Some(term_bytes[insert_pos - 1].utf8_to_string()?);
                    }
                },
            }

            if cfg!(feature = "test_log_verbose") {
                println!("  expected={:?}", expected);
            }

            test_floor(&mut compiled, &s, expected.as_deref())?;
        }

        Ok(())
    }
    #[test]
    fn test_random() -> Result<()> {
        let mut random = random();
        let num_terms = at_least(&mut random, 400);
        let mut terms = HashSet::new();

        while terms.len() < num_terms as usize {
            terms.insert(random_string(&mut random));
        }
        let term_vec: Vec<&str> = terms.iter().map(|s| s.as_str()).collect();

        test_terms(&mut random, num_terms * 100, &term_vec)?;

        Ok(())
    }

    fn random_string<R: Rng + ?Sized>(random: &mut R) -> String {
        TestUtil::random_realistic_unicode_string(random)
    }
    #[test]
    fn test_basic() -> Result<()> {
        let mut compiled = build(
            Operations::DEFAULT_DETERMINIZE_WORK_LIMIT as i32,
            &["fob", "foo", "goo"],
        )?;

        test_floor(&mut compiled, "goo", Some("goo"))?;
        test_floor(&mut compiled, "ga", Some("foo"))?;
        test_floor(&mut compiled, "g", Some("foo"))?;
        test_floor(&mut compiled, "foc", Some("fob"))?;
        test_floor(&mut compiled, "foz", Some("foo"))?;
        test_floor(&mut compiled, "f", None)?;
        test_floor(&mut compiled, "", None)?;
        test_floor(&mut compiled, "aa", None)?;
        test_floor(&mut compiled, "zzz", Some("goo"))?;

        Ok(())
    }
    // LUCENE-6367
    #[test]
    fn test_binary_all() -> Result<()> {
        let mut a = Automaton::new();
        let state = a.create_state();
        a.set_accept(state, true);
        a.add_transition(state, state, 0, 0xff)?;
        a.finish_state()?;

        let ca = CompiledAutomaton::with_binary(a, false, true, true)?;

        assert_eq!(ca.automaton_type, AutomatonType::All);
        Ok(())
    }
    // LUCENE-6367
    #[test]
    fn test_unicode_all() -> Result<()> {
        let mut a = Automaton::new();
        let state = a.create_state();
        a.set_accept(state, true);
        a.add_transition(state, state, 0, char::MAX as i32)?;
        a.finish_state()?;

        let ca = CompiledAutomaton::with_binary(a, false, true, false)?;
        assert_eq!(ca.automaton_type, AutomatonType::All);

        Ok(())
    }
    // LUCENE-6367
    #[test]
    fn test_binary_singleton() -> Result<()> {
        let a = Automata::make_string("foobar")?;
        let ca = CompiledAutomaton::with_binary(a, true, true, true)?;
        assert_eq!(ca.automaton_type, AutomatonType::Single);
        Ok(())
    }
    // LUCENE-6367
    #[test]
    fn test_unicode_singleton() -> Result<()> {
        let mut random = random();
        let s = TestUtil::random_realistic_unicode_string(&mut random);
        let a = Automata::make_string(&s)?;
        let ca = CompiledAutomaton::with_binary(a, true, true, false)?;
        assert_eq!(ca.automaton_type, AutomatonType::Single);
        Ok(())
    }
}
