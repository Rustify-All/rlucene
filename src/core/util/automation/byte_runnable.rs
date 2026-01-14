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
use crate::core::util::automation::byte_run_automaton::ByteRunAutomaton;
use crate::core::util::automation::nfa_run_automaton::NFARunAutomaton;
use crate::core::util::error::lucene_error::Result;
use std::rc::Rc;

/// A runnable automaton accepting byte array as input
pub trait ByteRunnable {
    /// Returns the state obtained by reading the given byte from the given
    /// state.
    ///
    /// Returns -1 if not obtaining any such state.
    ///
    /// # Parameters
    /// - `state`: the last state
    /// - `c`: the input codepoint
    ///
    /// # Returns
    /// The next state, or -1 if no such transition.
    fn step(&self, state: i32, c: i32) -> i32;

    /// Returns acceptance status for given state.
    ///
    /// # Parameters
    /// - `state`: the state
    ///
    /// # Returns
    /// Whether the state is accepted.
    fn is_accept(&self, state: i32) -> Result<bool>;

    /// Returns number of states this automaton has.
    ///
    /// Note: This may not be an accurate number in case of an NFA.
    ///
    /// # Returns
    /// Number of states.
    fn get_size(&self) -> i32;

    /// Returns true if the given byte array is accepted by this automaton.
    ///
    /// # Parameters
    /// - `s`: input byte slice
    /// - `offset`: start index
    /// - `length`: number of bytes to read
    ///
    /// # Returns
    /// Whether the automaton accepts the input.
    fn run(&self, s: &[u8], offset: usize, length: usize) -> Result<bool> {
        let mut p = 0;
        let end = offset + length;
        for &b in &s[offset..end] {
            p = self.step(p, b as i32);
            if p == -1 {
                return Ok(false);
            }
        }
        self.is_accept(p)
    }
}

pub enum ByteRunnableEnum {
    Byte(Rc<ByteRunAutomaton>),
    NFA(Rc<NFARunAutomaton>),
}
impl ByteRunnable for ByteRunnableEnum {
    fn step(&self, state: i32, c: i32) -> i32 {
        match self {
            ByteRunnableEnum::Byte(bra) => bra.step(state, c),
            ByteRunnableEnum::NFA(nfa) => nfa.step(state, c),
        }
    }

    fn is_accept(&self, state: i32) -> Result<bool> {
        match self {
            ByteRunnableEnum::Byte(bra) => bra.is_accept(state),
            ByteRunnableEnum::NFA(nfa) => nfa.is_accept(state),
        }
    }

    fn get_size(&self) -> i32 {
        match self {
            ByteRunnableEnum::Byte(bra) => bra.get_size(),
            ByteRunnableEnum::NFA(nfa) => nfa.get_size(),
        }
    }

    fn run(&self, s: &[u8], offset: usize, length: usize) -> Result<bool> {
        match self {
            ByteRunnableEnum::Byte(bra) => bra.run(s, offset, length),
            ByteRunnableEnum::NFA(nfa) => nfa.run(s, offset, length),
        }
    }
}
