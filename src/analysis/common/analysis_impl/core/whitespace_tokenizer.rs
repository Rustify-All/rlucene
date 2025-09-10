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
use crate::core::analysis::token_stream::{TokenStream, default_attribute};
use crate::core::analysis::tokenizer::{Tokenizer, TokenizerBase};
use crate::core::analysis::util::char_tokenizer::{CharTokenizer, CharTokenizerBase};
use crate::core::util::attribute_source::Attributes;
use crate::core::util::error::lucene_error::Result;
/// A tokenizer that divides text at whitespace characters as defined by
/// `Char::is_whitespace(int)`.
/// Note: That definition explicitly excludes the non-breaking space.
/// Adjacent sequences of non-whitespace characters form tokens.
pub struct WhitespaceTokenizer {
    base: CharTokenizerBase,
}
impl WhitespaceTokenizer {
    pub fn new() -> Result<Self> {
        Ok(WhitespaceTokenizer {
            base: CharTokenizerBase::new()?,
        })
    }
    /// Construct a new WhitespaceTokenizer using a given [`Attributes`]
    pub fn with_att(att: Attributes) -> Result<Self> {
        Ok(WhitespaceTokenizer {
            base: CharTokenizerBase::with_att(att)?,
        })
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
    pub fn with_max_token_len(max_token_len: i32) -> Result<Self> {
        Ok(WhitespaceTokenizer {
            base: CharTokenizerBase::with_max_token_len(default_attribute(), max_token_len)?,
        })
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
    pub fn with_max_token_len_and_att(att: Attributes, max_token_len: i32) -> Result<Self> {
        Ok(WhitespaceTokenizer {
            base: CharTokenizerBase::with_max_token_len(att, max_token_len)?,
        })
    }
}

impl Tokenizer for WhitespaceTokenizer {
    fn get_tokenizer_base_mut(&mut self) -> &mut TokenizerBase {
        &mut self.base.tokenizer_base
    }

    fn get_tokenizer_base(&self) -> &TokenizerBase {
        &self.base.tokenizer_base
    }
}

impl TokenStream for WhitespaceTokenizer {
    fn increment_token(&mut self) -> Result<bool> {
        self.base
            .increment_token_with(|c| !c.is_whitespace())
    }

    fn end(&mut self) -> Result<()> {
        self.base.end()
    }

    fn reset(&mut self) -> Result<()> {
        self.base.reset()
    }

    fn close(&mut self) -> Result<()> {
        self.base.close()
    }

    type AttributeSource = Attributes;

    fn get_attribute_source(&self) -> &Self::AttributeSource {
        &self.base.att
    }

    fn get_attribute_source_mut(&mut self) -> &mut Self::AttributeSource {
        &mut self.base.att
    }
}

impl CharTokenizer for WhitespaceTokenizer {
    /// Collects only characters which do not satisfy Char::is_whitespace().
    fn is_token_char(&self, c: &char) -> bool {
        !c.is_whitespace()
    }
}
