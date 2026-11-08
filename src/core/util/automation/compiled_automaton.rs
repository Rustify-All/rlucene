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
use crate::core::codecs::block_term_state::TermStateEnum;
use crate::core::index::filtered_terms_enum::FilteredTermsEnum;
use crate::core::index::impacts_enum::ImpactsEnumEnum2;
use crate::core::index::single_terms_enum::SingleTermsEnum;
use crate::core::index::term::Term;
use crate::core::index::terms::{Terms, TermsIntersect, TermsPosting, TermsTE};
use crate::core::index::terms_enum::{EmptyTermsEnumTermsWrapper, SeekStatus, TermsEnum};
use crate::core::index::{BytesRef, BytesRefBuilder, BytesRefValue, BytesRefValueEnum};
use crate::core::search::query::QueryRef;
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::util::StringHelper;
use crate::core::util::access::ByteSource;
use crate::core::util::accountable::Accountable;
use crate::core::util::attribute_source::AttributeSourceEnum2;
use crate::core::util::automation::automata::Automata;
use crate::core::util::automation::automaton::Automaton;
use crate::core::util::automation::byte_run_automaton::ByteRunAutomaton;
use crate::core::util::automation::byte_runnable::ByteRunnable;
use crate::core::util::automation::nfa_run_automaton::NFARunAutomaton;
use crate::core::util::automation::operations::Operations;
use crate::core::util::automation::transition::Transition;
use crate::core::util::automation::transition_accessor::TransitionAccessor;
use crate::core::util::automation::utf32_to_utf8::UTF32ToUTF8;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::ram_usage_estimator::size_of_vec;
use crate::core::util::unicode_util::UnicodeUtil;
use smallvec::SmallVec;
use std::borrow::Cow;
use std::hash::{Hash, Hasher};
use std::mem;
use std::sync::Arc;

/// Automata are compiled into different internal forms for the most efficient
/// execution depending upon the language they accept.
#[derive(Clone)]
pub struct CompiledAutomaton {
  /// If `simplify` is true this will be the "simplified" type; else, this is
  /// NORMAL
  pub type_: AutomatonType,

  /// For [`AutomatonType::Single`] this is the singleton term.
  pub term: Option<BytesRef<Vec<u8>>>,

  /// Matcher for quickly determining whether a byte slice is accepted. Only valid for
  /// [`AutomatonType::Normal`].
  pub run_automaton: Option<ByteRunAutomaton>,

  /// Matcher directly run on a NFA, it will determinize the state on need and
  /// caches it, note that this field and
  /// [`CompiledAutomaton::run_automaton`] will not be present at the same
  /// time.
  ///
  pub(crate) nfa_run_automaton: Option<NFARunAutomaton>,

  /// Shared common suffix accepted by the automaton. Only valid for
  /// [`AutomatonType::Normal`], and only when the automaton accepts an
  /// infinite language. This will be `None` if the common prefix is
  /// length 0.
  pub common_suffix_ref: Option<Arc<BytesRef<Vec<u8>>>>,

  /// Indicates if the automaton accepts a finite set of strings.
  /// Only valid for [`AutomatonType::Normal`].
  pub finite: bool,

  /// Which state, if any, accepts all suffixes, else -1.
  pub sink_state: i32,

  transition: Transition,
}
impl CompiledAutomaton {
  /// Creates a [`CompiledAutomaton`] with `finite = false` and `simplify =
  /// true`.
  pub fn from_automaton(automaton: Automaton) -> Result<Self> {
    Self::with_binary(automaton, false, true, false)
  }

  pub fn visit<QV>(&self, visitor: &mut QV, parent: QueryRef<'_>, field: &str) -> Result<()>
  where
    QV: QueryVisitor,
  {
    if visitor.accept_field(field) {
      match self.type_ {
        AutomatonType::Normal => {
          visitor.consume_terms_matching(parent, field, || Ok(self.run_automaton.clone()))?;
        },
        AutomatonType::None => {},
        AutomatonType::All => {
          visitor.consume_terms_matching(parent, field, || {
            Ok(Some(ByteRunAutomaton::new(Automata::make_any_string()?)?))
          })?;
        },
        AutomatonType::Single => {
          visitor.consume_terms(
            parent,
            &[Term::new(
              field,
              self
                .term
                .clone()
                .ok_or_else(|| LuceneError::illegal_state("missing singleton term"))?,
            )],
          )?;
        },
      }
    }
    Ok(())
  }
  /// Returns sink state, if present, else -1.
  fn find_sink_state(automaton: &Automaton) -> Result<i32> {
    let num_states = automaton.get_num_states();
    let mut t = Transition::default();
    for s in 0..num_states {
      if automaton.is_accept(s) {
        let count = automaton.init_transition(s, &mut t);
        for _ in 0..count {
          automaton.get_next_transition(&mut t)?;
          if t.dest == s && t.min == 0 && t.max == 0xff {
            return Ok(s);
          }
        }
      }
    }
    Ok(-1)
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
      automaton.create_state()?;
    }
    // simplify requires a DFA
    if simplify && automaton.is_deterministic() {
      // Test whether the automaton is a "simple" form and
      // if so, don't create a runAutomaton.  Note that on a
      // large automaton these tests could be costly:
      if Operations::is_empty(&automaton)? {
        return Ok(Self {
          type_: AutomatonType::None,
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
          type_: AutomatonType::All,
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
          Some(BytesRef::from_bytes(
            UnicodeUtil::new_string(
              singleton.ints.as_slice(),
              singleton.offset,
              singleton.length,
            )?
            .into_bytes(),
          ))
        };

        return Ok(Self {
          type_: AutomatonType::Single,
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
          Some(Arc::new(suffix))
        }
      };

    if !automaton_is_deterministic && !binary.is_deterministic() {
      Ok(Self {
        type_: automaton_type,
        term,
        run_automaton: None,

        nfa_run_automaton: Some(NFARunAutomaton::with_alphabet_size(binary, 0xff)?),
        common_suffix_ref,
        finite,
        sink_state: -1,
        transition: Transition::default(),
      })
    } else {
      // We already had a DFA (or threw error), according to mike UTF32toUTF8
      // won't "blow up"
      let dfa = match Operations::determinize(&binary, i32::MAX as usize)? {
        Cow::Borrowed(_) => binary,
        Cow::Owned(o) => o,
      };
      let run_automaton = ByteRunAutomaton::with_bool(dfa, true)?;
      let sink_state = Self::find_sink_state(&run_automaton.base.tables.automaton)?;

      Ok(Self {
        type_: automaton_type,
        term,
        run_automaton: Some(run_automaton),
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
    let mut max_index = None;
    let run_automaton = self
      .run_automaton
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("normal compiled automaton is missing its DFA"))?;
    let automaton = &run_automaton.base.tables.automaton;
    let num_transitions = automaton.init_transition(state, &mut self.transition);
    for i in 0..num_transitions {
      automaton.get_next_transition(&mut self.transition)?;
      if self.transition.min < lead_label {
        max_index = Some(i);
      } else {
        // Transitions are always sorted
        break;
      }
    }

    let max_index = max_index.ok_or_else(|| {
      LuceneError::illegal_state("no transition below the lead label exists in add_tail")
    })?;
    automaton.get_transition(state, max_index, &mut self.transition);
    // Append floorLabel
    let floor_label = if self.transition.max > lead_label - 1 {
      lead_label - 1
    } else {
      self.transition.max
    };

    term.grow(idx + 1)?;
    term.set_byte_at(idx, floor_label as u8);
    state = self.transition.dest;
    idx += 1;

    loop {
      let num_transitions = automaton.get_num_transitions_with_state(state);

      if num_transitions == 0 {
        debug_assert!(run_automaton.is_accept(state)?);
        term.set_length(idx);
        return Ok(term.get_bytes_owner());
      }

      automaton.get_transition(state, num_transitions - 1, &mut self.transition);
      term.grow(idx + 1)?;
      term.set_byte_at(idx, self.transition.max as u8);
      state = self.transition.dest;
      idx += 1;
    }
  }
  pub fn get_terms_enum<T>(&self, terms: T) -> Result<CompiledAutomatonTE<T>>
  where
    T: Terms,
  {
    let v = match self.type_ {
      AutomatonType::None => CompiledAutomatonTE::Empty(EmptyTermsEnumTermsWrapper::new(terms)),
      AutomatonType::All => CompiledAutomatonTE::TE(terms.iterator()?),
      AutomatonType::Single => {
        let term = self
          .term
          .as_ref()
          .ok_or_else(|| LuceneError::illegal_state("term must exist for AutomatonType::Single"))?;
        let terms_enum = terms.iterator()?;
        CompiledAutomatonTE::Single(SingleTermsEnum::new(terms_enum, term.clone()))
      },
      AutomatonType::Normal => CompiledAutomatonTE::Intersect(terms.intersect(self, None)?),
    };
    Ok(v)
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
    let run_automaton = self
      .run_automaton
      .as_mut()
      .ok_or_else(|| LuceneError::illegal_state("normal compiled automaton is missing its DFA"))?;
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
    let mut stack = SmallVec::<[i32; 8]>::new();

    loop {
      let mut label = input.bytes[input.offset + idx] as i32;
      let mut next_state = run_automaton.step(state, label)?;

      if idx == input.length - 1 {
        if next_state != -1 && run_automaton.is_accept(next_state)? {
          output.grow(idx + 1)?;
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
          let num_transitions = run_automaton
            .base
            .tables
            .automaton
            .get_num_transitions_with_state(state);
          if num_transitions == 0 {
            debug_assert!(run_automaton.is_accept(state)?);
            output.set_length(idx);
            return Ok(Some(output.get_bytes_owner()));
          } else {
            run_automaton
              .base
              .tables
              .automaton
              .get_transition(state, 0, &mut self.transition);
            if label - 1 < self.transition.min {
              if run_automaton.is_accept(state)? {
                output.set_length(idx);
                return Ok(Some(output.get_bytes_owner()));
              }
              let Some(previous_state) = stack.pop() else {
                return Ok(None);
              };
              state = previous_state;
              idx -= 1;
              label = input.bytes[input.offset + idx] as i32;
            } else {
              break;
            }
          }
        }
        return Ok(Some(self.add_tail(state, output, idx, label)?));
      } else {
        output.grow(idx + 1)?;
        output.set_byte_at(idx, label as u8);
        if !stack.spilled() && stack.len() == stack.capacity() {
          stack.reserve_exact(input.length - stack.len());
        }
        stack.push(state);
        state = next_state;
        idx += 1;
      }
    }
  }
  /// Returns a [`ByteRunnable`] instance, which differs depending on whether
  /// an NFA or DFA is passed in. This method does not guarantee returning
  /// a present object.
  #[allow(dead_code)]
  pub fn get_byte_runnable(&self) {
    // use get_automaton instead of this
  }
  /// Returns a [`TransitionAccessor`] instance, which varies depending on
  /// whether an NFA or DFA is passed in. This method does not guarantee
  /// returning a present object.
  #[allow(dead_code)]
  pub fn get_transition_accessor(&self) {
    // use get_automaton instead of this
  }
  pub fn get_automaton(&self) -> Result<AutomatonEnum> {
    match (self.run_automaton.as_ref(), self.nfa_run_automaton.as_ref()) {
      (Some(_), Some(_)) => Err(LuceneError::illegal_state(
        "Both run_automaton and nfa_run_automaton are non-null",
      )),
      (Some(run), None) => Ok(AutomatonEnum::Byte(run.clone())),
      (None, Some(nfa)) => Ok(AutomatonEnum::NFA(Box::new(nfa.clone()))),
      (None, None) => Err(LuceneError::illegal_state(
        "Both run_automaton and nfa_run_automaton are None,, should not be called",
      )),
    }
  }
}

impl Accountable for CompiledAutomaton {
  fn ram_bytes_used(&self) -> Result<i64> {
    let mut size = 0i64;

    if let Some(term) = &self.term {
      size = size.saturating_add(size_of_vec(&term.bytes));
    }
    if let Some(run_automaton) = &self.run_automaton {
      size = size.saturating_add(run_automaton.ram_bytes_used()?);
    }
    if let Some(nfa_run_automaton) = &self.nfa_run_automaton {
      size = size.saturating_add(nfa_run_automaton.ram_bytes_used()?);
    }
    if let Some(common_suffix_ref) = &self.common_suffix_ref {
      size = size
        .saturating_add(mem::size_of_val(common_suffix_ref.as_ref()) as i64)
        .saturating_add(size_of_vec(&common_suffix_ref.bytes));
    }
    size = size.saturating_add(self.transition.ram_bytes_used()?);

    Ok(size)
  }
}
impl Hash for CompiledAutomaton {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    self.type_.hash(state);
    match self.type_ {
      AutomatonType::Single => {
        self.term.hash(state);
      },
      AutomatonType::Normal => {
        self.run_automaton.hash(state);
        hash_opt_ref_identity(&self.nfa_run_automaton, state);
      },
      AutomatonType::All | AutomatonType::None => {},
    }
  }
}
fn hash_opt_ref_identity<T, H>(v: &Option<T>, state: &mut H)
where
  H: Hasher,
{
  match v.as_ref() {
    None => 0usize.hash(state),
    Some(value) => {
      (value as *const T as usize).hash(state);
    },
  }
}
impl PartialEq for CompiledAutomaton {
  fn eq(&self, other: &Self) -> bool {
    if self.type_ != other.type_ {
      return false;
    }

    match self.type_ {
      AutomatonType::Single => self.term == other.term,
      AutomatonType::Normal => {
        self.run_automaton == other.run_automaton
          && match (
            self.nfa_run_automaton.as_ref(),
            other.nfa_run_automaton.as_ref(),
          ) {
            (None, None) => true,
            (Some(a), Some(b)) => std::ptr::eq(a, b),
            _ => false,
          }
      },
      AutomatonType::All | AutomatonType::None => true,
    }
  }
}

impl Eq for CompiledAutomaton {}
/// Automata are compiled into different internal forms for the most efficient
/// execution depending upon the language they accept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

pub enum AutomatonEnum {
  Byte(ByteRunAutomaton),
  NFA(Box<NFARunAutomaton>),
}
impl AutomatonEnum {
  // -----Implement ByteRunnable----
  pub fn step(&mut self, state: i32, c: i32) -> Result<i32> {
    match self {
      AutomatonEnum::Byte(bra) => bra.step(state, c),
      AutomatonEnum::NFA(nfa) => nfa.step(state, c),
    }
  }

  pub fn is_accept(&self, state: i32) -> Result<bool> {
    match self {
      AutomatonEnum::Byte(bra) => bra.is_accept(state),
      AutomatonEnum::NFA(nfa) => nfa.is_accept(state),
    }
  }

  pub fn get_size(&self) -> Result<usize> {
    match self {
      AutomatonEnum::Byte(bra) => Ok(bra.get_size()),
      AutomatonEnum::NFA(nfa) => Ok(nfa.get_size()),
    }
  }

  pub fn run(&mut self, s: &[u8], offset: usize, length: usize) -> Result<bool> {
    match self {
      AutomatonEnum::Byte(bra) => bra.run(s, offset, length),
      AutomatonEnum::NFA(nfa) => ByteRunnable::run(nfa.as_mut(), s, offset, length),
    }
  }
  // -----Implement TransitionAccessor----

  pub fn init_transition(&mut self, state: i32, t: &mut Transition) -> Result<i32> {
    match self {
      AutomatonEnum::Byte(byte) => Ok(byte.base.tables.automaton.init_transition(state, t)),
      AutomatonEnum::NFA(nfa) => nfa.init_transition(state, t),
    }
  }

  pub fn get_next_transition(&mut self, t: &mut Transition) -> Result<()> {
    match self {
      AutomatonEnum::Byte(byte) => byte.base.tables.automaton.get_next_transition(t),
      AutomatonEnum::NFA(nfa) => nfa.get_next_transition(t),
    }
  }

  pub fn get_num_transitions_with_state(&mut self, state: i32) -> Result<i32> {
    match self {
      AutomatonEnum::Byte(byte) => Ok(
        byte
          .base
          .tables
          .automaton
          .get_num_transitions_with_state(state),
      ),
      AutomatonEnum::NFA(nfa) => nfa.get_num_transitions_with_state(state),
    }
  }

  pub fn get_transition(&mut self, state: i32, index: i32, t: &mut Transition) -> Result<()> {
    match self {
      AutomatonEnum::Byte(byte) => {
        byte.base.tables.automaton.get_transition(state, index, t);
        Ok(())
      },
      AutomatonEnum::NFA(nfa) => nfa.get_transition(state, index, t),
    }
  }
}

pub enum CompiledAutomatonTE<T>
where
  T: Terms,
{
  Empty(EmptyTermsEnumTermsWrapper<T>),
  TE(TermsTE<T>),
  Single(FilteredTermsEnum<TermsTE<T>, SingleTermsEnum>),
  Intersect(TermsIntersect<T>),
}

impl<T> BytesRefIterator for CompiledAutomatonTE<T>
where
  T: Terms,
  TermsIntersect<T>: TermsEnum<PostingsEnum = TermsPosting<T>>,
{
  type Value<'a>
    = BytesRefValueEnum<'a>
  where
    Self: 'a;

  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    match self {
      Self::Empty(t) => t.next().map(|value| value.map(BytesRefValue::into_value)),
      Self::TE(t) => t.next().map(|value| value.map(BytesRefValue::into_value)),
      Self::Single(t) => t.next().map(|value| value.map(BytesRefValue::into_value)),
      Self::Intersect(t) => t.next().map(|value| value.map(BytesRefValue::into_value)),
    }
  }

  fn set_next(&mut self) -> Result<bool> {
    match self {
      Self::Empty(t) => t.set_next(),
      Self::TE(t) => t.set_next(),
      Self::Single(t) => t.set_next(),
      Self::Intersect(t) => t.set_next(),
    }
  }
}

impl<T> TermsEnum for CompiledAutomatonTE<T>
where
  T: Terms,
  TermsIntersect<T>: TermsEnum<PostingsEnum = TermsPosting<T>>,
{
  type AttributeSource<'a>
    = AttributeSourceEnum2<
    <TermsTE<T> as TermsEnum>::AttributeSource<'a>,
    <TermsIntersect<T> as TermsEnum>::AttributeSource<'a>,
  >
  where
    Self: 'a;
  type AttributeSourceMut<'a>
    = AttributeSourceEnum2<
    <TermsTE<T> as TermsEnum>::AttributeSourceMut<'a>,
    <TermsIntersect<T> as TermsEnum>::AttributeSourceMut<'a>,
  >
  where
    Self: 'a;

  fn attributes(&self) -> Result<Self::AttributeSource<'_>> {
    match self {
      Self::Empty(_) => Err(LuceneError::unsupported_operation("")),
      Self::TE(t) => Ok(AttributeSourceEnum2::A(t.attributes()?)),
      Self::Single(t) => Ok(AttributeSourceEnum2::A(t.attributes()?)),
      Self::Intersect(t) => Ok(AttributeSourceEnum2::B(t.attributes()?)),
    }
  }

  fn attributes_mut(&mut self) -> Result<Self::AttributeSourceMut<'_>> {
    match self {
      Self::Empty(_) => Err(LuceneError::unsupported_operation("")),
      Self::TE(t) => Ok(AttributeSourceEnum2::A(t.attributes_mut()?)),
      Self::Single(t) => Ok(AttributeSourceEnum2::A(t.attributes_mut()?)),
      Self::Intersect(t) => Ok(AttributeSourceEnum2::B(t.attributes_mut()?)),
    }
  }

  fn seek_exact<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<bool> {
    match self {
      Self::Empty(t) => t.seek_exact(term),
      Self::TE(t) => t.seek_exact(term),
      Self::Single(t) => t.seek_exact(term),
      Self::Intersect(t) => t.seek_exact(term),
    }
  }

  fn prepare_seek_exact<BS: ByteSource>(&mut self, text: &BytesRef<BS>) -> Result<Option<()>> {
    match self {
      Self::Empty(t) => t.prepare_seek_exact(text),
      Self::TE(t) => t.prepare_seek_exact(text),
      Self::Single(t) => t.prepare_seek_exact(text),
      Self::Intersect(t) => t.prepare_seek_exact(text),
    }
  }

  fn get_prepare_seek_exact_status<BS: ByteSource>(
    &mut self,
    target: &BytesRef<BS>,
  ) -> Result<bool> {
    match self {
      Self::Empty(t) => t.get_prepare_seek_exact_status(target),
      Self::TE(t) => t.get_prepare_seek_exact_status(target),
      Self::Single(t) => t.get_prepare_seek_exact_status(target),
      Self::Intersect(t) => t.get_prepare_seek_exact_status(target),
    }
  }

  fn seek_ceil<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<SeekStatus> {
    match self {
      Self::Empty(t) => t.seek_ceil(term),
      Self::TE(t) => t.seek_ceil(term),
      Self::Single(t) => t.seek_ceil(term),
      Self::Intersect(t) => t.seek_ceil(term),
    }
  }

  fn seek_exact_with_ord(&mut self, ord: i64) -> Result<()> {
    match self {
      Self::Empty(t) => t.seek_exact_with_ord(ord),
      Self::TE(t) => t.seek_exact_with_ord(ord),
      Self::Single(t) => t.seek_exact_with_ord(ord),
      Self::Intersect(t) => t.seek_exact_with_ord(ord),
    }
  }

  fn seek_exact_with_state<BS: ByteSource>(
    &mut self,
    term: &BytesRef<BS>,
    state: &TermStateEnum,
  ) -> Result<()> {
    match self {
      Self::Empty(t) => t.seek_exact_with_state(term, state),
      Self::TE(t) => t.seek_exact_with_state(term, state),
      Self::Single(t) => t.seek_exact_with_state(term, state),
      Self::Intersect(t) => t.seek_exact_with_state(term, state),
    }
  }

  fn term(&self) -> Result<Self::Value<'_>> {
    match self {
      Self::Empty(t) => t.term().map(BytesRefValue::into_value),
      Self::TE(t) => t.term().map(BytesRefValue::into_value),
      Self::Single(t) => t.term().map(BytesRefValue::into_value),
      Self::Intersect(t) => t.term().map(BytesRefValue::into_value),
    }
  }

  fn ord(&self) -> Result<i64> {
    match self {
      Self::Empty(t) => t.ord(),
      Self::TE(t) => t.ord(),
      Self::Single(t) => t.ord(),
      Self::Intersect(t) => t.ord(),
    }
  }

  fn doc_freq(&mut self) -> Result<i32> {
    match self {
      Self::Empty(t) => t.doc_freq(),
      Self::TE(t) => t.doc_freq(),
      Self::Single(t) => t.doc_freq(),
      Self::Intersect(t) => t.doc_freq(),
    }
  }

  fn total_term_freq(&mut self) -> Result<i64> {
    match self {
      Self::Empty(t) => t.total_term_freq(),
      Self::TE(t) => t.total_term_freq(),
      Self::Single(t) => t.total_term_freq(),
      Self::Intersect(t) => t.total_term_freq(),
    }
  }

  type PostingsEnum = TermsPosting<T>;

  fn postings(&mut self, reuse: Option<Self::PostingsEnum>) -> Result<Self::PostingsEnum> {
    match self {
      Self::Empty(t) => t.postings(reuse),
      Self::TE(t) => t.postings(reuse),
      Self::Single(t) => t.postings(reuse),
      Self::Intersect(t) => t.postings(reuse),
    }
  }

  fn postings_with_flags(
    &mut self,
    reuse: Option<Self::PostingsEnum>,
    flags: i32,
  ) -> Result<Self::PostingsEnum> {
    match self {
      Self::Empty(t) => t.postings_with_flags(reuse, flags),
      Self::TE(t) => t.postings_with_flags(reuse, flags),
      Self::Single(t) => t.postings_with_flags(reuse, flags),
      Self::Intersect(t) => t.postings_with_flags(reuse, flags),
    }
  }

  type ImpactsEnum = ImpactsEnumEnum2<
    <TermsTE<T> as TermsEnum>::ImpactsEnum,
    <TermsIntersect<T> as TermsEnum>::ImpactsEnum,
  >;

  fn impacts(&mut self, flags: i32) -> Result<Self::ImpactsEnum> {
    match self {
      Self::Empty(_) => Err(LuceneError::illegal_state(
        "this method should never be called",
      )),
      Self::TE(t) => t.impacts(flags).map(ImpactsEnumEnum2::A),
      Self::Single(t) => t.impacts(flags).map(ImpactsEnumEnum2::A),
      Self::Intersect(t) => t.impacts(flags).map(ImpactsEnumEnum2::B),
    }
  }

  fn term_state(&mut self) -> Result<TermStateEnum> {
    match self {
      Self::Empty(t) => t.term_state(),
      Self::TE(t) => t.term_state(),
      Self::Single(t) => t.term_state(),
      Self::Intersect(t) => t.term_state(),
    }
  }
}
