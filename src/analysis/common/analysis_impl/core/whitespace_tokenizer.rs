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
use crate::core::analysis::token_stream::default_attribute;
use crate::core::analysis::util::char_tokenizer::{CharTokenizer, CharTokenizerBase};
use crate::core::util::attribute_source::Attributes;
use crate::core::util::error::lucene_error::Result;
/// A tokenizer that divides text at whitespace characters as defined by
/// `Char::is_whitespace(int)`.
/// Note: That definition explicitly excludes the non-breaking space.
/// Adjacent sequences of non-whitespace characters form tokens.
pub struct WhitespaceTokenizer;
impl WhitespaceTokenizer {
    pub fn new() -> Result<CharTokenizer<WhitespaceTokenizer>> {
        CharTokenizer::new(WhitespaceTokenizer)
    }
    /// Construct a new WhitespaceTokenizer using a given [`Attributes`]
    pub fn with_att(att: Attributes) -> Result<CharTokenizer<WhitespaceTokenizer>> {
        CharTokenizer::with_att(att, WhitespaceTokenizer)
    }
    /// Constructs a new `WhitespaceTokenizer` using a given maximum token length.
    ///
    /// # Arguments
    ///
    /// * `max_token_len` — maximum token length the tokenizer will emit.
    ///   Must be greater than 0 and less than `MAX_TOKEN_LENGTH_LIMIT` (1024 * 1024).
    ///
    /// # Errors
    ///
    /// Returns an `IllegalArgumentException` if `max_token_len` is invalid.
    pub fn with_max_token_len(max_token_len: i32) -> Result<CharTokenizer<WhitespaceTokenizer>> {
        CharTokenizer::with_max_token_len(default_attribute(), max_token_len, WhitespaceTokenizer)
    }
    /// Constructs a new `WhitespaceTokenizer` using a given [`Attributes`].
    ///
    /// # Arguments
    ///
    /// * `max_token_len` — maximum token length the tokenizer will emit.
    ///   Must be greater than 0 and less than `MAX_TOKEN_LENGTH_LIMIT` (1024 * 1024).
    ///
    /// # Errors
    ///
    /// Returns an `IllegalArgumentException` if `max_token_len` is invalid.
    pub fn with_max_token_len_and_att(
        att: Attributes,
        max_token_len: i32,
    ) -> Result<CharTokenizer<WhitespaceTokenizer>> {
        CharTokenizer::with_max_token_len(att, max_token_len, WhitespaceTokenizer)
    }
}
impl CharTokenizerBase for WhitespaceTokenizer {
    fn is_token_char(&self, c: &char) -> bool {
        !c.is_whitespace()
    }
}
