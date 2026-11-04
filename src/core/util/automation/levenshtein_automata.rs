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

use crate::core::util::automation::automata::Automata;
use crate::core::util::automation::automaton::Automaton;
use crate::core::util::automation::lev1_parametric_description::Lev1ParametricDescription;
use crate::core::util::automation::lev1t_parametric_description::Lev1TParametricDescription;
use crate::core::util::automation::lev2_parametric_description::Lev2ParametricDescription;
use crate::core::util::automation::lev2t_parametric_description::Lev2TParametricDescription;
use crate::core::util::automation::operations::Operations;
use crate::core::util::automation::{
  lev1_parametric_description, lev1t_parametric_description, lev2_parametric_description,
  lev2t_parametric_description,
};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::impl_from_for_enum;

/// Constructs DFAs that match a word within some edit distance.
///
/// Implements the algorithm described in: Schulz and Mihov: Fast String Correction with
/// Levenshtein Automata.
pub struct LevenshteinAutomata {
  /// Input word.
  word: Vec<i32>,
  /// The automata alphabet.
  alphabet: smallvec::SmallVec<[i32; 8]>,
  /// The maximum symbol in the alphabet (e.g. 255 for UTF-8 or 10FFFF for UTF-32).
  #[allow(dead_code)]
  alpha_max: i32,
  /// Inclusive bounds for ranges outside of alphabet.
  ranges: smallvec::SmallVec<[(i32, i32); 4]>,
  num_ranges: usize,
  descriptions: [Option<ParametricDescription>; 3],
}

impl LevenshteinAutomata {
  /// Maximum edit distance this type can generate an automaton for.
  pub const MAXIMUM_SUPPORTED_DISTANCE: i32 = 2;

  /// Create a new [`LevenshteinAutomata`] for some input string. Optionally count transpositions as a
  /// primitive edit.
  pub fn new(input: &str, with_transpositions: bool) -> Result<Self> {
    Self::from_word(
      Self::code_points(input),
      char::MAX as i32,
      with_transpositions,
    )
  }

  /// Expert: specify a custom maximum possible symbol (`alpha_max`); default is
  /// `char::MAX`.
  pub fn from_word<W>(word: W, alpha_max: i32, with_transpositions: bool) -> Result<Self>
  where
    W: Into<Vec<i32>>,
  {
    let word = word.into();
    let mut alphabet = smallvec::SmallVec::<[i32; 8]>::with_capacity(word.len());
    for &v in &word {
      if v > alpha_max {
        return Err(LuceneError::illegal_argument(format!(
          "alphaMax exceeded by symbol {v} in word"
        )));
      }
      alphabet.push(v);
    }
    alphabet.sort_unstable();
    alphabet.dedup();
    alphabet.shrink_to_fit();

    let mut ranges = smallvec::SmallVec::<[(i32, i32); 4]>::with_capacity(alphabet.len() + 1);
    ranges.resize(alphabet.len() + 1, (0, 0));
    let mut num_ranges = 0;

    let mut lower = 0;
    for &higher in &alphabet {
      if higher > lower {
        ranges[num_ranges] = (lower, higher - 1);
        num_ranges += 1;
      }
      lower = higher + 1;
    }

    if lower <= alpha_max {
      ranges[num_ranges] = (lower, alpha_max);
      num_ranges += 1;
    }

    let w = word.len() as i32;
    let descriptions = [
      None,
      Some(if with_transpositions {
        lev1t_parametric_description::new(w)
      } else {
        lev1_parametric_description::new(w)
      }),
      Some(if with_transpositions {
        lev2t_parametric_description::new(w)
      } else {
        lev2_parametric_description::new(w)
      }),
    ];

    Ok(Self {
      word,
      alphabet,
      alpha_max,
      ranges,
      num_ranges,
      descriptions,
    })
  }

  fn code_points(input: &str) -> Vec<i32> {
    input.chars().map(|ch| ch as i32).collect()
  }

  /// Compute a DFA that accepts all strings within an edit distance of `n`.
  ///
  /// All automata have the following properties:
  ///
  /// - They are deterministic (DFA).
  /// - There are no transitions to dead states.
  /// - They are not minimal (some transitions could be combined).
  pub fn to_automaton(&self, n: usize) -> Result<Option<Automaton>> {
    self.to_automaton_with_prefix(n, "")
  }

  /// Compute a DFA that accepts all strings within an edit distance of `n`, matching the specified
  /// exact prefix.
  ///
  /// All automata have the following properties:
  ///
  /// - They are deterministic (DFA).
  /// - There are no transitions to dead states.
  /// - They are not minimal (some transitions could be combined).
  pub fn to_automaton_with_prefix(&self, n: usize, prefix: &str) -> Result<Option<Automaton>> {
    if n == 0 {
      if prefix.is_empty()
        && self
          .word
          .iter()
          .all(|&cp| char::from_u32(cp as u32).is_some())
      {
        return Ok(Some(Automata::make_string_from_codepoints(
          &self.word,
          0,
          self.word.len(),
        )?));
      }
      let mut word = String::with_capacity(prefix.len() + self.word.len());
      word.push_str(prefix);
      for &cp in &self.word {
        match char::from_u32(cp as u32) {
          Some(ch) => word.push(ch),
          None => {
            return Err(LuceneError::illegal_argument(format!(
              "invalid Unicode code point {cp} in word"
            )));
          },
        }
      }
      return Ok(Some(Automata::make_string(&word)?));
    }

    if n >= self.descriptions.len() {
      return Ok(None);
    }

    let Some(description) = &self.descriptions[n] else {
      return Ok(None);
    };
    let range = 2 * n as i32 + 1;
    let num_states = description.size();
    let num_transitions = num_states * std::cmp::min(1 + 2 * n as i32, self.alphabet.len() as i32);
    let prefix_states = prefix.chars().count() as i32;

    let mut a = Automaton::with_capacity(
      (num_states + prefix_states) as usize,
      num_transitions as usize,
    );
    let mut last_state = a.create_state()?;
    for cp in prefix.chars().map(|ch| ch as i32) {
      let state = a.create_state()?;
      a.add_transition_label(last_state, state, cp)?;
      last_state = state;
    }

    let state_offset = last_state;
    a.set_accept(last_state, description.is_accept(0));

    for i in 1..num_states {
      let state = a.create_state()?;
      a.set_accept(state, description.is_accept(i));
    }

    for k in 0..num_states {
      let xpos = description.get_position(k);
      if xpos < 0 {
        continue;
      }
      let end = xpos + std::cmp::min(self.word.len() as i32 - xpos, range);

      for &ch in &self.alphabet {
        let cvec = self.get_vector(ch, xpos, end);
        let dest = description.transition(k, xpos, cvec);
        if dest >= 0 {
          a.add_transition_label(state_offset + k, state_offset + dest, ch)?;
        }
      }
      // add transitions for all other chars in unicode
      // by definition, their characteristic vectors are always 0,
      // because they do not exist in the input string.
      let dest = description.transition(k, xpos, 0);
      if dest >= 0 {
        for r in 0..self.num_ranges {
          a.add_transition(
            state_offset + k,
            state_offset + dest,
            self.ranges[r].0,
            self.ranges[r].1,
          )?;
        }
      }
    }

    a.finish_state()?;
    let automaton = Operations::remove_dead_states(&a)?.into_owned();
    debug_assert!(automaton.is_deterministic());
    Ok(Some(automaton))
  }

  /// Get the characteristic vector `X(x, V)` where V is `substring(pos, end)`.
  fn get_vector(&self, x: i32, pos: i32, end: i32) -> i32 {
    let mut vector = 0;
    for i in pos..end {
      vector <<= 1;
      if self.word[i as usize] == x {
        vector |= 1;
      }
    }
    vector
  }
}

/// A `ParametricDescription` describes the structure of a Levenshtein DFA for some degree `n`.
///
/// There are four components of a parametric description, all parameterized on the length of
/// the word `w`:
///
/// 1. The number of states: [`ParametricDescription::size`]
/// 2. The set of final states: [`ParametricDescription::is_accept`]
/// 3. The transition function: [`ParametricDescription::transition`]
/// 4. Minimal boundary function: [`ParametricDescription::get_position`]
pub(crate) struct ParametricDescription {
  pub(crate) w: i32,
  n: i32,
  min_errors: &'static [i32],
  sub: ParametricDescriptionBaseEnum,
}

impl ParametricDescription {
  pub(crate) fn new<T>(w: i32, n: i32, min_errors: &'static [i32], sub: T) -> Self
  where
    T: Into<ParametricDescriptionBaseEnum>,
  {
    Self {
      w,
      n,
      min_errors,
      sub: sub.into(),
    }
  }
  /// Return the number of states needed to compute a Levenshtein DFA.
  pub(crate) fn size(&self) -> i32 {
    self.min_errors.len() as i32 * (self.w + 1)
  }

  /// Returns `true` if the `state` in any Levenshtein DFA is an accept state (final state).
  pub(crate) fn is_accept(&self, abs_state: i32) -> bool {
    // decode absState -> state, offset
    let state = abs_state / (self.w + 1);
    let offset = abs_state % (self.w + 1);
    debug_assert!(offset >= 0);

    self.w - offset + self.min_errors[state as usize] <= self.n
  }

  /// Returns the position in the input word for a given `state`. This is the minimal boundary for
  /// the state.
  pub(crate) fn get_position(&self, abs_state: i32) -> i32 {
    abs_state % (self.w + 1)
  }

  pub(crate) fn transition(&self, state: i32, position: i32, vector: i32) -> i32 {
    self.sub.transition(state, position, vector, self)
  }
}
pub(crate) trait ParametricDescriptionBase {
  /// Returns the state number for a transition from the given `state`, assuming `position` and
  /// characteristic vector `vector`.
  fn transition(&self, state: i32, position: i32, vector: i32, base: &ParametricDescription)
  -> i32;
}
pub(crate) enum ParametricDescriptionBaseEnum {
  Lev1(Lev1ParametricDescription),
  Lev1T(Lev1TParametricDescription),
  Lev2(Lev2ParametricDescription),
  Lev2T(Lev2TParametricDescription),
}
impl_from_for_enum!(
ParametricDescriptionBaseEnum,
Lev1ParametricDescription=> Lev1,
Lev1TParametricDescription=> Lev1T,
Lev2ParametricDescription=> Lev2,
Lev2TParametricDescription=> Lev2T,
);
impl ParametricDescriptionBase for ParametricDescriptionBaseEnum {
  fn transition(
    &self,
    state: i32,
    position: i32,
    vector: i32,
    base: &ParametricDescription,
  ) -> i32 {
    match self {
      ParametricDescriptionBaseEnum::Lev1(lev1) => lev1.transition(state, position, vector, base),
      ParametricDescriptionBaseEnum::Lev1T(lev1t) => {
        lev1t.transition(state, position, vector, base)
      },
      ParametricDescriptionBaseEnum::Lev2(lev2) => lev2.transition(state, position, vector, base),
      ParametricDescriptionBaseEnum::Lev2T(lev2t) => {
        lev2t.transition(state, position, vector, base)
      },
    }
  }
}
const MASKS: [i64; 63] = [
  0x1,
  0x3,
  0x7,
  0xf,
  0x1f,
  0x3f,
  0x7f,
  0xff,
  0x1ff,
  0x3ff,
  0x7ff,
  0xfff,
  0x1fff,
  0x3fff,
  0x7fff,
  0xffff,
  0x1ffff,
  0x3ffff,
  0x7ffff,
  0xfffff,
  0x1fffff,
  0x3fffff,
  0x7fffff,
  0xffffff,
  0x1ffffff,
  0x3ffffff,
  0x7ffffff,
  0xfffffff,
  0x1fffffff,
  0x3fffffff,
  0x7fffffff,
  0xffffffff,
  0x1ffffffff,
  0x3ffffffff,
  0x7ffffffff,
  0xfffffffff,
  0x1fffffffff,
  0x3fffffffff,
  0x7fffffffff,
  0xffffffffff,
  0x1ffffffffff,
  0x3ffffffffff,
  0x7ffffffffff,
  0xfffffffffff,
  0x1fffffffffff,
  0x3fffffffffff,
  0x7fffffffffff,
  0xffffffffffff,
  0x1ffffffffffff,
  0x3ffffffffffff,
  0x7ffffffffffff,
  0xfffffffffffff,
  0x1fffffffffffff,
  0x3fffffffffffff,
  0x7fffffffffffff,
  0xffffffffffffff,
  0x1ffffffffffffff,
  0x3ffffffffffffff,
  0x7ffffffffffffff,
  0xfffffffffffffff,
  0x1fffffffffffffff,
  0x3fffffffffffffff,
  0x7fffffffffffffff,
];
pub(crate) fn unpack(data: &[i64], index: i32, bits_per_value: usize) -> i32 {
  let bit_loc = bits_per_value as i64 * index as i64;
  let data_loc = (bit_loc >> 6) as usize;
  let bit_start = (bit_loc & 63) as usize;

  if bit_start + bits_per_value <= 64 {
    ((data[data_loc] >> bit_start) & MASKS[bits_per_value - 1]) as i32
  } else {
    let part = 64 - bit_start;
    (((data[data_loc] >> bit_start) & MASKS[part - 1])
      + ((data[1 + data_loc] & MASKS[bits_per_value - part - 1]) << part)) as i32
  }
}
