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
use std::char::decode_utf16;

use crate::core::util::automation::automaton::Automaton;
use crate::core::util::automation::run_automaton::RunAutomaton;
use crate::core::util::error::lucene_error::Result;

/// Automaton representation for matching char[].
pub struct CharacterRunAutomaton {
    pub base: RunAutomaton,
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
        let utf16_vec: Vec<u16> = s.encode_utf16().collect();
        let length = utf16_vec.len();
        self.run_chars(utf16_vec.as_slice(), 0, length)
    }

    /// Returns true if the given UTF-16 `char` buffer is accepted.
    pub fn run_chars(&self, chars: &[u16], offset: usize, length: usize) -> Result<bool> {
        let mut state: i32 = 0;

        let iter = decode_utf16(chars[offset..offset + length].iter().cloned());

        for result in iter {
            match result {
                Ok(ch) => {
                    state = self.base.step(state, ch as i32);
                    if state == -1 {
                        return Ok(false);
                    }
                },
                Err(_) => return Ok(false),
            }
        }

        self.base.is_accept(state)
    }
}
