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
use std::fmt;
use std::hash::Hash;

use crate::core::util::number::Number;
/// Expert: Describes the score computation for document and query.
#[derive(Clone)]
pub struct Explanation {
    pub matched: bool,
    pub value: Number,
    pub description: String,
    pub details: Vec<Explanation>,
}
impl Explanation {
    /// Internal constructor, equivalent to private constructor in Java
    fn new<N, S>(matched: bool, value: N, description: S, details: Vec<Explanation>) -> Self
    where
        N: Into<Number>,
        S: Into<String>,
    {
        Explanation {
            matched,
            value: value.into(),
            description: description.into(),
            details,
        }
    }
    /// Indicates whether or not this Explanation models a match.
    pub fn is_match(&self) -> bool {
        self.matched
    }
    /// The value assigned to this explanation node.
    pub fn get_value(&self) -> &Number {
        &self.value
    }
    /// A description of this explanation node.
    pub fn get_description(&self) -> &str {
        &self.description
    }

    fn get_summary(&self) -> String {
        format!("{} = {}", self.get_value(), self.get_description())
    }
    /// The sub-nodes of this explanation node.
    pub fn get_details(&self) -> &[Explanation] {
        &self.details
    }
    /// Render an explanation as text.
    fn to_string_with_depth(&self, depth: usize) -> String {
        let mut buffer = String::new();
        for _ in 0..depth {
            buffer.push_str("  ");
        }
        buffer.push_str(&self.get_summary());
        buffer.push('\n');

        for detail in &self.details {
            buffer.push_str(&detail.to_string_with_depth(depth + 1));
        }

        buffer
    }
    /// Create a new explanation for a match.
    ///
    /// # Arguments
    ///
    /// * `value` - The contribution to the score of the document.
    /// * `description` - How `value` was computed.
    /// * `details` - Sub explanations that contributed to this explanation.
    pub fn match_<N, S>(value: N, description: S, details: Vec<Explanation>) -> Explanation
    where
        N: Into<Number>,
        S: Into<String>,
    {
        Explanation::new(true, value, description, details)
    }
    /// Create a new explanation for a document which does not match.
    pub fn no_match<S>(description: S, details: Vec<Explanation>) -> Explanation
    where
        S: Into<String>,
    {
        Explanation::new(false, Number::F32(0.0), description, details)
    }
}
impl PartialEq for Explanation {
    fn eq(&self, other: &Self) -> bool {
        self.matched == other.matched
            && self.value == other.value
            && self.description == other.description
            && self.details == other.details
    }
}
impl Hash for Explanation {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.matched.hash(state);
        self.value.hash(state);
        self.description.hash(state);
        self.details.hash(state);
    }
}

impl Eq for Explanation {}
impl fmt::Display for Explanation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_string_with_depth(0))
    }
}
