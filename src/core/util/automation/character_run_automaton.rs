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
use crate::core::util::automation::automaton::Automaton;
use crate::core::util::automation::run_automaton::RunAutomaton;
use crate::core::util::error::lucene_error::Result;
use std::char::decode_utf16;
use std::hash::{Hash, Hasher};

/// Automaton representation for matching char[].
pub struct CharacterRunAutomaton {
  pub base: RunAutomaton,
}
#[cfg(test)]
impl Clone for CharacterRunAutomaton {
  fn clone(&self) -> Self {
    Self {
      base: self.base.clone(),
    }
  }
}
impl CharacterRunAutomaton {
  /// Constructs the automaton. error if the input is not deterministic.
  pub fn new(automaton: Automaton) -> Result<Self> {
    Ok(Self {
      base: RunAutomaton::new(automaton, char::MAX as usize + 1)?,
    })
  }

  /// Returns true if the given string is accepted by this automaton.
  pub fn run_str(&self, s: &str) -> Result<bool> {
    let mut state = 0;
    for ch in s.chars() {
      state = self.base.step(state, ch as i32);
      if state == -1 {
        return Ok(false);
      }
    }
    self.base.is_accept(state)
  }

  /// Returns true if the given UTF-16 code-unit slice is accepted.
  pub fn run_chars(&self, chars: &[u16], offset: usize, length: usize) -> Result<bool> {
    let mut state: i32 = 0;

    let iter = decode_utf16(chars[offset..offset + length].iter().cloned());

    for result in iter {
      // Java's codePointAt returns an unpaired surrogate as its own code unit.
      let code_point = match result {
        Ok(ch) => ch as i32,
        Err(error) => i32::from(error.unpaired_surrogate()),
      };
      state = self.base.step(state, code_point);
      if state == -1 {
        return Ok(false);
      }
    }

    self.base.is_accept(state)
  }
}
impl PartialEq for CharacterRunAutomaton {
  fn eq(&self, other: &Self) -> bool {
    self.base.eq(&other.base)
  }
}
impl Eq for CharacterRunAutomaton {}
impl Hash for CharacterRunAutomaton {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    self.base.hash(state)
  }
}
