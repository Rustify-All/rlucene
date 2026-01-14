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
use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use crate::core::index::index_reader::Identity;
use crate::core::index::{BytesRef, BytesRefBuilder};
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::automation::automata::Automata;
use crate::core::util::automation::automaton::{Automaton, Builder};
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::unicode_util::{UTF8CodePoint, UnicodeUtil};
/// Builds a minimal, deterministic [`Automaton`] that accepts a set of strings
/// using the algorithm described in [Incremental Construction of Minimal Acyclic Finite-State Automata by Daciuk, Mihov, Watson and Watson](https://aclanthology.org/J00-1002.pdf).
///
/// This requires sorted input data, but is very fast (nearly linear with the
/// input size). Also offers the ability to directly build a binary
/// [`Automaton`] representation. Users should access this functionality through
/// [`Automata`] static methods.
///
/// See also:
/// - [`Automata::make_string_union_bytes`](Automaton::make_string_union_bytes)
/// - [`Automata::make_binary_string_union`](Automaton::make_binary_string_union)
/// - [`Automata::make_string_union_iter`](Automaton::make_string_union_iter)
/// - [`Automata::make_binary_string_union_iter`](Automaton::make_binary_string_union_iter)
pub(crate) struct StringsToAutomaton {
    // TODO IMPORTANT 这里没必要用RcRefCell包装吧
    /// A "registry" for state interning.
    pub(crate) state_registry: Option<HashMap<StateKey, Rc<RefCell<State>>>>,
    /// Root automaton state.
    pub(crate) root: Rc<RefCell<State>>,
    /// Used for input order checking (only through assertions right now)
    pub(crate) previous: Option<BytesRefBuilder<Vec<u8>>>,
}

impl StringsToAutomaton {
    pub(crate) fn new() -> Self {
        StringsToAutomaton {
            state_registry: Some(HashMap::new()),
            root: Rc::new(RefCell::new(State::new())),
            previous: None,
        }
    }
    /// Copies `current` into an internal buffer.
    fn set_previous(&mut self, current: &BytesRef<Vec<u8>>) {
        match &mut self.previous {
            Some(prev) => {
                prev.copy_bytes_with_ref(current);
            },
            None => {
                let mut builder = BytesRefBuilder::new();
                builder.copy_bytes_with_ref(current);
                self.previous = Some(builder);
            },
        }
    }
    /// Internal recursive traversal for conversion.
    fn convert(
        a: &mut Builder,
        s: &Rc<RefCell<State>>,
        visited: &mut HashMap<Identity, i32>,
    ) -> Result<i32> {
        let key = s.borrow().identity.clone();

        if let Some(&converted) = visited.get(&key) {
            return Ok(converted);
        }

        let converted = a.create_state();
        {
            let s = s.borrow();
            a.set_accept(converted, s.is_final);
        }

        visited.insert(key, converted);
        let s = s.borrow();

        for (i, target) in s.states.iter().enumerate() {
            let v = Self::convert(a, target, visited)?;
            a.add_transition_label(converted, v, s.labels[i]);
        }

        Ok(converted)
    }
    /// Called after adding all terms. Performs final minimization and converts
    /// to a standard [`Automaton`] instance.
    fn complete_and_convert(&mut self) -> Result<Automaton> {
        if self.state_registry.is_none() {
            return Err(LuceneError::illegal_state(""));
        }

        {
            if self.root.borrow().has_children() {
                self.replace_or_register(self.root.clone())?;
            }
        }

        self.state_registry = None;

        let mut a = Builder::new();
        Self::convert(&mut a, &self.root, &mut HashMap::new())?;
        a.finish()
    }
    /// Builds a minimal, deterministic automaton from a sorted list of
    /// [`BytesRef`] representing strings in UTF-8. These strings must be
    /// binary-sorted. Creates an [`Automaton`] with either UTF-8 codepoints
    /// as transition labels or binary (compiled) transition labels based on
    /// `as_binary`.
    pub(crate) fn build(input: &[BytesRef<Vec<u8>>], as_binary: bool) -> Result<Automaton> {
        let mut builder = StringsToAutomaton::new();

        for b in input {
            builder.add(b, as_binary)?;
        }

        builder.complete_and_convert()
    }
    /// Builds a minimal, deterministic automaton from a sorted list of
    /// [`BytesRef`] representing strings in UTF-8. These strings must be
    /// binary-sorted. Creates an [`Automaton`] with either UTF-8 codepoints
    /// as transition labels or binary (compiled) transition labels based on
    /// `as_binary`.
    pub(crate) fn build_from_iterator<B>(input: &mut B, as_binary: bool) -> Result<Automaton>
    where
        B: BytesRefIterator,
    {
        let mut builder = StringsToAutomaton::new();

        while let Some(b) = input.next()? {
            builder.add(&b, as_binary)?; // b: Cow<'_, BytesRef<Vec<u8>>> ->
            // &BytesRef<Vec<u8>>
        }

        builder.complete_and_convert()
    }

    fn add(&mut self, current: &BytesRef<Vec<u8>>, as_binary: bool) -> Result<()> {
        if current.length > Automata::MAX_STRING_UNION_TERM_LENGTH as usize {
            return Err(LuceneError::illegal_argument(format!(
                "This builder doesn't allow terms that are larger than {} UTF-8 bytes, got {:?}",
                Automata::MAX_STRING_UNION_TERM_LENGTH,
                current
            )));
        }

        debug_assert!(self.state_registry.is_some(), "Automaton already built.");

        if let Some(prev) = &mut self.previous
            && prev.bytes_ref.cmp(current) == std::cmp::Ordering::Greater
        {
            return Err(LuceneError::illegal_argument(format!(
                "Input must be in sorted UTF-8 order: {} >= {}",
                prev.bytes_ref, current
            )));
        }
        self.set_previous(current);
        let mut code_point = UTF8CodePoint::default();

        let bytes = &current.bytes;
        let mut pos = current.offset;
        let max = current.offset + current.length;
        let mut state = Rc::clone(&self.root);
        let mut next;

        if as_binary {
            while pos < max {
                let b = bytes[pos] as i32;
                next = state.borrow().last_child_with_label(b);
                if let Some(child) = next {
                    state = child;
                    pos += 1;
                } else {
                    break;
                }
            }
        } else {
            while pos < max {
                code_point = *UnicodeUtil::code_point_at(bytes, pos, &mut code_point)?;
                next = state.borrow().last_child_with_label(code_point.code_point);
                if let Some(child) = next {
                    state = child;
                    pos += code_point.num_bytes;
                } else {
                    break;
                }
            }
        }

        if state.borrow().has_children() {
            self.replace_or_register(Rc::clone(&state))?;
        }

        if as_binary {
            while pos < max {
                let b = bytes[pos] as i32;
                let new_state = state.borrow_mut().new_state(b)?;
                state = new_state;
                pos += 1;
            }
        } else {
            while pos < max {
                code_point = *UnicodeUtil::code_point_at(bytes, pos, &mut code_point)?;
                let new_state = state.borrow_mut().new_state(code_point.code_point)?;
                state = new_state;
                pos += code_point.num_bytes;
            }
        }

        state.borrow_mut().is_final = true;

        Ok(())
    }
    /// Replaces the last child of `state` with an already registered state or
    /// registers the last child state into the state registry.
    fn replace_or_register(&mut self, state: Rc<RefCell<State>>) -> Result<()> {
        let child = state.borrow().last_child();

        if child.borrow().has_children() {
            self.replace_or_register(child.clone())?;
        }
        let state_key = StateKey {
            state: Rc::clone(&child),
        };
        if let Some(registered) = self.state_registry.as_ref().unwrap().get(&state_key) {
            state.borrow_mut().replace_last_child(Rc::clone(registered));
        } else {
            self.state_registry.as_mut().unwrap().insert(
                StateKey {
                    state: Rc::clone(&child),
                },
                Rc::clone(&child),
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct State {
    /// Labels of outgoing transitions. Indexed identically to [`states`].
    /// Labels must be sorted lexicographically.
    pub labels: Vec<i32>,
    /// States reachable from outgoing transitions. Indexed identically to
    /// [`labels`].
    pub states: Vec<Rc<RefCell<State>>>,
    /// `true` if this state corresponds to the end of at least one input
    /// sequence.
    pub is_final: bool,
    pub identity: Identity,
}
// for padding
impl Default for State {
    fn default() -> Self {
        Self {
            labels: Vec::new(),
            states: Vec::new(),
            is_final: false,
            identity: Identity::new(),
        }
    }
}

impl State {
    pub(crate) fn new() -> Self {
        State {
            labels: Vec::new(),
            states: Vec::new(),
            is_final: false,
            identity: Identity::new(),
        }
    }
    /// Returns the target state of a transition leaving this state and labeled
    /// with `label`. If no such transition exists, returns `None`.
    pub(crate) fn get_state(&self, label: i32) -> Option<&Rc<RefCell<State>>> {
        match self.labels.binary_search(&label) {
            Ok(index) => self.states.get(index),
            Err(_) => None,
        }
    }
    /// Returns `true` if this state has any children (outgoing transitions).
    pub(crate) fn has_children(&self) -> bool {
        !self.labels.is_empty()
    }
    /// Creates a new outgoing transition labeled `label` and returns the newly
    /// created target state for this transition.
    pub(crate) fn new_state(&mut self, label: i32) -> Result<Rc<RefCell<State>>> {
        debug_assert!(
            self.labels.binary_search(&label).is_err(),
            "State already has transition labeled: {label}"
        );
        let mut labels_len = self.labels.len();
        let mut states_len = self.states.len();
        ArrayUtil::grow_exact(&mut self.labels, labels_len + 1)?;
        ArrayUtil::grow_exact(&mut self.states, states_len + 1)?;
        labels_len = self.labels.len();
        states_len = self.states.len();
        self.labels[labels_len - 1] = label;
        let new_state = Rc::new(RefCell::new(State::new()));
        self.states[states_len - 1] = new_state.clone();
        Ok(new_state)
    }
    /// Returns the most recent transition's target state.
    pub(crate) fn last_child(&self) -> Rc<RefCell<State>> {
        debug_assert!(self.has_children(), "No outgoing transitions.");
        Rc::clone(self.states.last().unwrap())
    }
    /// Returns the associated state if the most recent transition is labeled
    /// with `label`.
    pub(crate) fn last_child_with_label(&self, label: i32) -> Option<Rc<RefCell<State>>> {
        let index = self.labels.len() as i32 - 1;
        if index >= 0 && self.labels[index as usize] == label {
            Some(Rc::clone(self.states.last().unwrap()))
        } else {
            None
        }
    }
    /// Compares two lists of objects for reference equality.
    pub(crate) fn replace_last_child(&mut self, state: Rc<RefCell<State>>) {
        debug_assert!(self.has_children(), "No outgoing transitions.");
        let len = self.states.len();
        self.states[len - 1] = state;
    }
    /// Compares two lists of objects for reference equality.
    fn reference_equals<T>(a1: &[Rc<T>], a2: &[Rc<T>]) -> bool {
        if a1.len() != a2.len() {
            return false;
        }
        a1.iter().zip(a2.iter()).all(|(a, b)| Rc::ptr_eq(a, b))
    }
}

#[derive(Clone)]
pub(crate) struct StateKey {
    state: Rc<RefCell<State>>,
}
impl PartialEq for StateKey {
    /// Two states are equal if:
    ///
    /// - They have an identical number of outgoing transitions, labeled with
    ///   the same labels
    /// - Corresponding outgoing transitions lead to the same states (to states
    ///   with an identical right-language)
    fn eq(&self, other: &Self) -> bool {
        let state = self.state.borrow();
        let other = other.state.borrow();
        state.is_final == other.is_final
            && state.labels == other.labels
            && state.states.len() == other.states.len()
            && State::reference_equals(&state.states, &other.states)
    }
}

impl Eq for StateKey {}

impl Hash for StateKey {
    fn hash<H: Hasher>(&self, hasher: &mut H) {
        let sb = self.state.borrow();
        let mut h: usize = if sb.is_final { 1 } else { 0 };
        h ^= h.wrapping_mul(31).wrapping_add(sb.labels.len());
        for &c in &sb.labels {
            h ^= h.wrapping_mul(31).wrapping_add(c as usize);
        }
        for rc in &sb.states {
            h ^= Rc::as_ptr(rc) as usize;
        }
        h.hash(hasher);
    }
}
#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::collections::HashSet;

    use rand::Rng;

    use crate::core::index::{BytesRef, BytesRefBuilder};
    use crate::core::util::array_util::ArrayUtil;
    use crate::core::util::automation::automata::Automata;
    use crate::core::util::automation::automaton::Automaton;
    use crate::core::util::automation::byte_runnable::ByteRunnable;
    use crate::core::util::automation::compiled_automaton::CompiledAutomaton;
    use crate::core::util::automation::finite_strings_iterator::{
        FiniteStringsIterator, FiniteStringsIteratorBase,
    };
    use crate::core::util::automation::operations::Operations;
    use crate::core::util::automation::strings_to_automaton::StringsToAutomaton;
    use crate::core::util::bytes_ref_iterator::BytesRefIterator;
    use crate::core::util::error::lucene_error::{LuceneError, Result};
    use crate::core::util::fst_impl::util::Util;
    use crate::test::util::automaton::automaton_test_util::AutomatonTestUtil;
    use crate::test::util::automaton::minimization_operation::MinimizationOperations;
    use crate::test::util::lucene_test_case::lucene_test_case_util::{
        is_night_mode, new_bytes_ref_from_bytes_ref, new_bytes_ref_from_string, random,
    };
    use crate::test::util::test_util::TestUtil;

    #[allow(dead_code)] // for quick search
    struct TestStringsToAutomaton;
    #[test]
    fn test_basic() -> Result<()> {
        let mut random = random();
        let mut terms = basic_terms(&mut random)?;
        terms.sort();

        let a = build(&mut random, terms.clone(), false)?;
        check_automaton(&terms, a.clone(), false)?;
        check_minimized(&a)?;

        Ok(())
    }
    #[test]
    fn test_basic_binary() -> Result<()> {
        let mut random = random();
        let mut terms = basic_terms(&mut random)?;
        terms.sort();

        let a = build(&mut random, terms.clone(), true)?;
        check_automaton(&terms, a.clone(), true)?;
        check_minimized(&a)?;

        Ok(())
    }

    #[test]
    fn test_random_minimized() -> Result<()> {
        let mut random = random();
        let iters = if is_night_mode() { 20 } else { 5 };

        for _ in 0..iters {
            let build_binary = false;
            let size = 2;

            let mut terms = Vec::new();
            let mut automaton_list = vec![];

            for _ in 0..size {
                if build_binary {
                    let t = TestUtil::random_binary_term_with_len(&mut random, 8);
                    automaton_list.push(Automata::make_binary(&t)?);
                    terms.push(t);
                } else {
                    let s = TestUtil::random_realistic_unicode_string_with_len(&mut random, 8);
                    let t = new_bytes_ref_from_string(&mut random, &s)?;
                    automaton_list.push(Automata::make_string(&s)?);
                    terms.push(t);
                }
            }

            let a = Operations::union_list(&automaton_list.iter().collect::<Vec<_>>())?;
            let expected =
                MinimizationOperations::minimize(&a, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;

            terms.sort_unstable();
            let actual = build(&mut random, terms, build_binary)?;

            assert_same_automaton(&expected, &actual)?;
        }

        Ok(())
    }
    #[test]
    fn test_random_unicode_only() -> Result<()> {
        let mut random = random();
        test_random(&mut random, false)
    }

    #[test]
    fn test_random_binary() -> Result<()> {
        let mut random = random();
        test_random(&mut random, true)
    }
    #[test]
    fn test_large_terms() -> Result<()> {
        let mut random = random();
        let b10k = vec![b'a'; 10_000];

        let result = build(&mut random, vec![BytesRef::from_bytes(b10k.clone())], false);
        assert!(
            matches!(result, Err(LuceneError::IllegalArgument(msg)) if msg.message.starts_with(
                &format!(
                    "This builder doesn't allow terms that are larger than {} UTF-8 bytes",
                    Automata::MAX_STRING_UNION_TERM_LENGTH
                )
            ))
        );

        let b1k = ArrayUtil::copy_of_sub_array(&b10k, 0, 1000);
        build(&mut random, vec![BytesRef::from_bytes(b1k)], false)?; // should not panic

        Ok(())
    }

    fn test_random<R: Rng + ?Sized>(random: &mut R, allow_binary: bool) -> Result<()> {
        let iters = if is_night_mode() { 50 } else { 10 };

        for _ in 0..iters {
            let size = random.random_range(500..2000);
            let mut terms = HashSet::with_capacity(size);

            let mut j = 0;
            while j < size {
                if allow_binary && random.random_range(0..10) < 2 {
                    // Sometimes random bytes term that isn't necessarily valid unicode
                    let v = TestUtil::random_binary_term(random);
                    terms.insert(new_bytes_ref_from_bytes_ref(random, &v)?);
                } else {
                    let s = TestUtil::random_realistic_unicode_string(random);
                    terms.insert(new_bytes_ref_from_string(random, &s)?);
                }
                j += 1;
            }

            let mut sorted: Vec<_> = terms.into_iter().collect();
            sorted.sort_unstable();

            let a = build(random, sorted.clone(), allow_binary)?;
            check_automaton(&sorted, a, allow_binary)?;
        }

        Ok(())
    }

    fn check_automaton(
        expected: &[BytesRef<Vec<u8>>],
        a: Automaton,
        is_binary: bool,
    ) -> Result<()> {
        let c = CompiledAutomaton::with_binary(a, true, false, is_binary)?;
        let run_automaton = c.run_automaton.as_ref().unwrap();

        // Make sure every expected term is accepted
        for t in expected {
            let readable = if is_binary {
                format!("{:?}", t.bytes)
            } else {
                t.utf8_to_string()?
            };

            assert!(
                run_automaton.run(&t.bytes, t.offset, t.length)?,
                "{} should be found but wasn't",
                readable
            );
        }

        // Make sure every term produced by the automaton is expected
        let mut scratch = BytesRefBuilder::new();
        let mut it = FiniteStringsIterator::new(&c.run_automaton.as_ref().unwrap().base.automaton);
        while let Some(r) = it.next()? {
            let t = Util::get_bytes_ref(&r, &mut scratch)?;
            assert!(
                expected.iter().any(|x| x == &t),
                "Unexpected term found: {:?}",
                t.utf8_to_string()?
            );
        }

        Ok(())
    }

    fn check_minimized(a: &Automaton) -> Result<()> {
        let minimized =
            MinimizationOperations::minimize(a, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;
        assert_same_automaton(&minimized, a)?;
        Ok(())
    }
    fn assert_same_automaton(a: &Automaton, b: &Automaton) -> Result<()> {
        assert_eq!(a.get_num_states(), b.get_num_states());
        assert_eq!(a.get_num_transitions(), b.get_num_transitions());
        assert!(AutomatonTestUtil::same_language(a, b)?);
        Ok(())
    }

    fn basic_terms<R: Rng + ?Sized>(random: &mut R) -> Result<Vec<BytesRef<Vec<u8>>>> {
        Ok(vec![
            new_bytes_ref_from_string(random, "dog")?,
            new_bytes_ref_from_string(random, "day")?,
            new_bytes_ref_from_string(random, "dad")?,
            new_bytes_ref_from_string(random, "cats")?,
            new_bytes_ref_from_string(random, "cat")?,
        ])
    }

    fn build<R: Rng + ?Sized>(
        random: &mut R,
        terms: Vec<BytesRef<Vec<u8>>>,
        as_binary: bool,
    ) -> Result<Automaton> {
        if random.random_bool(0.5) {
            StringsToAutomaton::build(terms.as_slice(), as_binary)
        } else {
            StringsToAutomaton::build_from_iterator(
                &mut TermIterator {
                    it: terms.into_iter(),
                },
                as_binary,
            )
        }
    }

    struct TermIterator {
        it: std::vec::IntoIter<BytesRef<Vec<u8>>>,
    }
    impl BytesRefIterator for TermIterator {
        fn next(&mut self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
            match self.it.next() {
                Some(b) => Ok(Some(Cow::Owned(b))),
                None => Ok(None),
            }
        }
    }
}
