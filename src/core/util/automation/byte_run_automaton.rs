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

use crate::core::util::automation::automaton::Automaton;
use crate::core::util::automation::byte_runnable::ByteRunnable;
use crate::core::util::automation::operations::Operations;
use crate::core::util::automation::run_automaton::RunAutomaton;
use crate::core::util::automation::utf32_to_utf8::UTF32ToUTF8;
use crate::core::util::error::lucene_error::{LuceneError, Result};

pub struct ByteRunAutomaton {
    pub base: RunAutomaton,
}

impl ByteRunAutomaton {
    /// Converts the incoming automaton to a byte-based one (via UTF-32 to UTF-8
    /// conversion).
    ///
    /// Errors:
    /// - Returns an error if the automaton is not deterministic.
    pub fn with_bool(a: Automaton, is_binary: bool) -> Result<Self> {
        let automaton = if is_binary {
            a
        } else {
            match Self::convert(&a)? {
                Cow::Borrowed(_) => a,
                Cow::Owned(o) => o,
            }
        };

        Ok(ByteRunAutomaton {
            base: RunAutomaton::new(automaton, 256)?,
        })
    }
    /// Expert use only: if `is_binary` is `true`, the input is already
    /// byte-based.
    ///
    /// Errors:
    /// - Returns an error if the automaton is not deterministic.
    pub fn new(a: Automaton) -> Result<Self> {
        Self::with_bool(a, false)
    }

    fn convert(a: &Automaton) -> Result<Cow<'_, Automaton>> {
        if !a.is_deterministic() {
            return Err(LuceneError::illegal_argument(
                "Automaton must be deterministic",
            ));
        }
        let converted = UTF32ToUTF8::default().convert(a)?;
        match Operations::determinize(&converted, i32::MAX as usize)? {
            Cow::Borrowed(_) => Ok(converted),
            Cow::Owned(o) => Ok(Cow::Owned(o)),
        }
    }
}
impl ByteRunnable for ByteRunAutomaton {
    fn step(&self, state: i32, c: i32) -> i32 {
        self.base.step(state, c)
    }

    fn is_accept(&self, state: i32) -> Result<bool> {
        self.base.is_accept(state)
    }

    fn get_size(&self) -> i32 {
        self.base.size()
    }
}
