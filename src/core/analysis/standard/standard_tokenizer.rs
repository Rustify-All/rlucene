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
use crate::core::analysis::reader::ReaderEnum;
use crate::core::analysis::standard::standard_analyzer::DEFAULT_MAX_TOKEN_LENGTH;
use crate::core::analysis::standard::standard_tokenizer_impl::{StandardTokenizerImpl, YYEOF};
use crate::core::analysis::token_stream::{TokenStream, default_attribute};
use crate::core::analysis::tokenizer::{Tokenizer, TokenizerBase};
use crate::core::util::attribute_source::{AttributeSource, Attributes};
use crate::core::util::close::Closeable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
/// A grammar-based tokenizer constructed with JFlex.
///
/// This struct implements the Word Break rules from the Unicode Text
/// Segmentation algorithm, as specified in
/// [Unicode Standard Annex #29](http://unicode.org/reports/tr29/).
///
/// Many applications have specific tokenizer requirements. If this tokenizer
/// does not suit your application, consider copying this source code directory
/// into your project and maintaining your own grammar-based tokenizer.
pub struct StandardTokenizer {
  /// A private instance of the JFlex-constructed scanner
  scanner: StandardTokenizerImpl,
  skipped_positions: i32,
  max_token_length: usize,
  tokenizer_base: TokenizerBase,
}
/// Alpha/numeric token type.
pub const ALPHANUM: i32 = 0;

/// Numeric token type.
pub const NUM: i32 = 1;

/// Southeast Asian token type.
pub const SOUTHEAST_ASIAN: i32 = 2;

/// Ideographic token type.
pub const IDEOGRAPHIC: i32 = 3;

/// Hiragana token type.
pub const HIRAGANA: i32 = 4;

/// Katakana token type.
pub const KATAKANA: i32 = 5;

/// Hangul token type.
pub const HANGUL: i32 = 6;

/// Emoji token type.
pub const EMOJI: i32 = 7;

/// Absolute maximum sized token
pub const MAX_TOKEN_LENGTH_LIMIT: usize = 1024 * 1024;
/// String token types corresponding to `i32` token-type constants.
pub const TOKEN_TYPES: [&str; 8] = [
  "<ALPHANUM>",
  "<NUM>",
  "<SOUTHEAST_ASIAN>",
  "<IDEOGRAPHIC>",
  "<HIRAGANA>",
  "<KATAKANA>",
  "<HANGUL>",
  "<EMOJI>",
];

impl StandardTokenizer {
  /// Creates a new instance of [`StandardTokenizer`].
  ///
  /// Attaches `input` to the newly created JFlex scanner.
  ///
  /// See <http://issues.apache.org/jira/browse/LUCENE-1068>
  pub fn new() -> Result<Self> {
    Ok(Self::with_att(default_attribute()?))
  }

  pub fn with_att(att: Attributes) -> Self {
    let tokenizer_base = TokenizerBase::new(att);
    let scanner = StandardTokenizerImpl::new(ReaderEnum::default());
    Self {
      scanner,
      skipped_positions: 0,
      max_token_length: DEFAULT_MAX_TOKEN_LENGTH,
      tokenizer_base,
    }
  }
  /// Sets the maximum allowed token length.
  ///
  /// Tokens longer than this value will be split at this length and emitted as
  /// multiple tokens. To skip such large tokens instead, you can increase this
  /// limit and then use `LengthFilter` to remove long tokens. The default value
  /// is [`DEFAULT_MAX_TOKEN_LENGTH`].
  ///
  /// # Errors
  ///
  /// Returns an error if the given length is outside the range
  /// `[1, MAX_TOKEN_LENGTH_LIMIT]`.
  pub fn set_max_token_length(&mut self, length: usize) -> Result<()> {
    if length < 1 {
      return Err(LuceneError::illegal_argument(
        "maxTokenLength must be greater than zero",
      ));
    } else if length > MAX_TOKEN_LENGTH_LIMIT {
      return Err(LuceneError::illegal_argument(format!(
        "maxTokenLength may not exceed {MAX_TOKEN_LENGTH_LIMIT}"
      )));
    }
    if length != self.max_token_length {
      self.max_token_length = length;
      self.scanner.set_buffer_size(length);
    }
    Ok(())
  }
  /// Returns the current maximum token length
  pub fn get_max_token_length(&self) -> usize {
    self.max_token_length
  }
}

impl Closeable for StandardTokenizer {
  fn close(&mut self) -> Result<()> {
    self.tokenizer_base.close()?;
    self.scanner.yyreset(self.tokenizer_base.input.clone());
    Ok(())
  }
}

impl TokenStream for StandardTokenizer {
  fn increment_token(&mut self) -> Result<bool> {
    // A caller may replace attributes between calls. Select for this call,
    // then keep the original character/token body concrete.
    macro_rules! increment_with_attributes {
      ($attributes:expr) => {{
        let att = $attributes;
        att.clear_attributes()?;
        self.skipped_positions = 0;

        loop {
          let token_type = self.scanner.get_next_token()?;
          if token_type == YYEOF {
            return Ok(false);
          }

          let length = self.scanner.match_length_units;
          if length <= self.max_token_length {
            {
              att.set_position_increment(self.skipped_positions + 1)?;
              self.scanner.get_text(att)?;
            }
            let start = self.scanner.yychar();
            let end = start + length as i32;
            let start = self.tokenizer_base.input.correct_offset(start);
            let end = self.tokenizer_base.input.correct_offset(end);
            att.set_offset(start, end)?;
            att.set_type(
              TOKEN_TYPES
                .get(token_type as usize)
                .ok_or_else(|| LuceneError::illegal_state("invalid token type"))?,
            )?;
            return Ok(true);
          }

          self.skipped_positions += 1;
        }
      }};
    }
    match &mut self.tokenizer_base.token_stream_base.att {
      Attributes::PackedToken(att) => increment_with_attributes!(att),
      Attributes::BytesTerm(att) => increment_with_attributes!(att),
      Attributes::BinaryTokenStream(att) => increment_with_attributes!(att),
      #[cfg(test)]
      Attributes::RandomTokenStream(att) => increment_with_attributes!(att),
    }
  }

  fn end(&mut self) -> Result<()> {
    self.tokenizer_base.end()?;
    let final_offset =
      self.correct_offset(self.scanner.yychar() + self.scanner.match_length_units as i32);
    let att = &mut self.tokenizer_base.token_stream_base.att;
    att.set_offset(final_offset, final_offset)?;
    let position_increment = att.get_position_increment()?;
    att.set_position_increment(position_increment + self.skipped_positions)
  }

  fn reset(&mut self) -> Result<()> {
    self.tokenizer_base.reset()?;
    self.scanner.yyreset(self.tokenizer_base.input.clone());
    self.skipped_positions = 0;
    Ok(())
  }

  fn get_attribute_source(&self) -> &Attributes {
    self.tokenizer_base.get_attribute_source()
  }

  fn get_attribute_source_mut(&mut self) -> &mut Attributes {
    self.tokenizer_base.get_attribute_source_mut()
  }

  fn set_reader(&mut self, input: ReaderEnum) -> Result<()> {
    self.tokenizer_base.set_reader(input)
  }
}

impl Tokenizer for StandardTokenizer {
  fn get_tokenizer_base_mut(&mut self) -> &mut TokenizerBase {
    &mut self.tokenizer_base
  }

  fn get_tokenizer_base(&self) -> &TokenizerBase {
    &self.tokenizer_base
  }
}
