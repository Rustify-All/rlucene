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

use crate::core::index::term_state::TermState;
use crate::core::util::error::lucene_error::Result;

/// An ordinal-based [`TermState`]
#[derive(Clone, Default)]
pub struct OrdTermState {
    /// Term ordinal, i.e. its position in the full list of sorted terms.
    pub ord: i64,
}
impl Display for OrdTermState {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ord={} ", std::any::type_name::<Self>(), self.ord)
    }
}

impl TermState for OrdTermState {
    fn copy_from(&mut self, other: &Self) -> Result<()> {
        self.ord = other.ord;
        Ok(())
    }
}
