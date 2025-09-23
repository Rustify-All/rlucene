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
use std::fmt::{Display, Formatter};

use crate::core::codecs::block_term_state::BlockTermStateEnum;
use crate::core::index::base_terms_enum::TermStateImpl1;
use crate::core::index::dummy::dummy_term_state_type::DummyTermState;
use crate::core::index::ord_term_state::OrdTermState;
use crate::core::util::error::lucene_error::{LuceneError, Result};

/// Encapsulates all required internal state to position the associated
/// [`TermsEnum`](crate::core::index::terms_enum::TermsEnum) without re-seeking.
pub trait TermState: Display + Clone {
    /// Copies the content of the given `TermState` to this instance.
    fn copy_from(&mut self, other: &Self) -> Result<()>;
}

pub enum TermStateEnum {
    Dummy(DummyTermState),
    Impl1(TermStateImpl1),
    Ord(OrdTermState),
    Block(BlockTermStateEnum),
}

impl Display for TermStateEnum {
    fn fmt(&self, _f: &mut Formatter<'_>) -> std::fmt::Result {
        todo!()
    }
}

impl Clone for TermStateEnum {
    fn clone(&self) -> Self {
        todo!()
    }
}

impl Default for TermStateEnum {
    fn default() -> Self {
        todo!()
    }
}

impl TermState for TermStateEnum {
    fn copy_from(&mut self, _other: &Self) -> Result<()> {
        todo!()
    }
}
// TermState
pub enum Either2TermState<A, B> {
    A(A),
    B(B),
}

impl<A, B> Display for Either2TermState<A, B>
where
    A: TermState,
    B: TermState,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Either2TermState::A(t) => write!(f, "EitherTermState::A({t})"),
            Either2TermState::B(s) => write!(f, "EitherTermState::B({s})"),
        }
    }
}

impl<A, B> Clone for Either2TermState<A, B>
where
    A: TermState,
    B: TermState,
{
    fn clone(&self) -> Self {
        match self {
            Either2TermState::A(t) => Either2TermState::A(t.clone()),
            Either2TermState::B(s) => Either2TermState::B(s.clone()),
        }
    }
}

impl<A, B> TermState for Either2TermState<A, B>
where
    A: TermState,
    B: TermState,
{
    fn copy_from(&mut self, other: &Self) -> Result<()> {
        match (self, other) {
            (Either2TermState::A(t), Either2TermState::A(o)) => t.copy_from(o),
            (Either2TermState::B(s), Either2TermState::B(o)) => s.copy_from(o),
            _ => Err(LuceneError::illegal_state(
                "TermState variants must match when copying",
            )),
        }
    }
}
