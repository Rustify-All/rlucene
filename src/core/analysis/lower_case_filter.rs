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
use crate::core::analysis::character_utils::CharacterUtils;
use crate::core::analysis::reader::ReaderEnum;
use crate::core::analysis::token_filter::{TokenFilter, TokenFilterBase};
use crate::core::analysis::token_stream::TokenStream;
use crate::core::util::attribute_source::{AttributeSource, Attributes};
use crate::core::util::close::Closeable;
use crate::core::util::error::lucene_error::Result;
/// Normalizes token text to lower case.
pub struct LowerCaseFilter<TS> {
  pub(crate) token_filter_base: TokenFilterBase<TS>,
}
impl<TS> LowerCaseFilter<TS> {
  /// Create a new `LowerCaseFilter` that normalizes token text to lower case.
  ///
  /// # Parameters
  ///
  /// - `in_`: [`TokenStream`](crate::core::analysis::token_stream::TokenStream) to filter.
  pub fn new(input: TS) -> Self {
    let token_filter_base = TokenFilterBase::new(input);
    Self { token_filter_base }
  }
}

impl<TS> Closeable for LowerCaseFilter<TS>
where
  TS: TokenStream,
{
  fn close(&mut self) -> Result<()> {
    self.token_filter_base.close()
  }
}

impl<TS> TokenStream for LowerCaseFilter<TS>
where
  TS: TokenStream,
{
  #[inline(always)]
  fn increment_token(&mut self) -> Result<bool> {
    if self.token_filter_base.input.increment_token()? {
      // Select the current attributes after each successful input token.
      macro_rules! lower_case {
        ($attr:expr) => {{
          let attr = $attr;
          let len = attr.length()?;
          CharacterUtils::convert_to_lower_case(attr.buffer_mut()?, 0, len);
        }};
      }
      match self.get_attribute_source_mut() {
        Attributes::PackedToken(attr) => lower_case!(attr),
        Attributes::BytesTerm(attr) => lower_case!(attr),
        Attributes::BinaryTokenStream(attr) => lower_case!(attr),
        #[cfg(test)]
        Attributes::RandomTokenStream(attr) => lower_case!(attr),
      }
      Ok(true)
    } else {
      Ok(false)
    }
  }

  fn end(&mut self) -> Result<()> {
    self.token_filter_base.end()
  }

  fn reset(&mut self) -> Result<()> {
    self.token_filter_base.reset()
  }

  fn set_reader(&mut self, input: ReaderEnum) -> Result<()> {
    self.token_filter_base.set_reader(input)
  }

  fn set_reader_test_point(&mut self) -> Result<()> {
    self.token_filter_base.set_reader_test_point()
  }

  fn get_attribute_source(&self) -> &Attributes {
    self.token_filter_base.input.get_attribute_source()
  }

  fn get_attribute_source_mut(&mut self) -> &mut Attributes {
    self.token_filter_base.input.get_attribute_source_mut()
  }
}

impl<TS> TokenFilter for LowerCaseFilter<TS> where TS: TokenStream {}
