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
use crate::core::analysis::token_filter::{TokenFilter, TokenFilterBase};
use crate::core::analysis::token_stream::TokenStream;
use crate::core::util::attribute_source::{AttributeSource, Attributes};
use crate::core::util::close::Closeable;
use crate::core::util::error::lucene_error::{LuceneError, Result};

pub struct FilteringTokenFilter<T, V>
where
  T: TokenStream,
  V: FilteringTokenFilterBase,
{
  skipped_positions: i32,
  pub(crate) base: TokenFilterBase<T>,
  sub: V,
}
impl<T, V> FilteringTokenFilter<T, V>
where
  T: TokenStream,
  V: FilteringTokenFilterBase,
{
  pub fn new(input: T, sub: V) -> Self {
    Self {
      skipped_positions: 0,
      base: TokenFilterBase::new(input),
      sub,
    }
  }
}

impl<T, V> Drop for FilteringTokenFilter<T, V>
where
  T: TokenStream,
  V: FilteringTokenFilterBase,
{
  fn drop(&mut self) {
    let _ = self.close();
  }
}

impl<T, V> Closeable for FilteringTokenFilter<T, V>
where
  T: TokenStream,
  V: FilteringTokenFilterBase,
{
  fn close(&mut self) -> Result<()> {
    self.base.close()
  }
}

impl<T, V> TokenStream for FilteringTokenFilter<T, V>
where
  T: TokenStream,
  V: FilteringTokenFilterBase,
{
  #[inline(always)]
  fn increment_token(&mut self) -> Result<bool> {
    self.skipped_positions = 0;
    {
      let att = self.base.input.get_attribute_source_mut();
      let missing_position_increment = match att {
        Attributes::PackedToken(_) => false,
        Attributes::BytesTerm(_) => true,
        Attributes::BinaryTokenStream(_) => false,
        #[cfg(test)]
        Attributes::RandomTokenStream(att) => att.get_position_increment().is_err(),
      };
      if missing_position_increment {
        return Err(LuceneError::illegal_state(
          "PositionIncrementAttribute is missing",
        ));
      }
    }
    loop {
      if !self.base.input.increment_token()? {
        break;
      }
      let att = self.base.input.get_attribute_source_mut();
      if self.sub.accept(att)? {
        if self.skipped_positions != 0 {
          let new_pos = att.get_position_increment()? + self.skipped_positions;
          att.set_position_increment(new_pos)?;
        }
        return Ok(true);
      }
      self.skipped_positions += att.get_position_increment()?;
    }
    Ok(false)
  }

  fn end(&mut self) -> Result<()> {
    self.base.end()?;
    let att = self.base.input.get_attribute_source_mut();
    // we can safely unwrap
    att.set_position_increment(att.get_position_increment()? + self.skipped_positions)?;
    Ok(())
  }

  fn reset(&mut self) -> Result<()> {
    self.base.reset()?;
    self.skipped_positions = 0;
    Ok(())
  }

  fn set_reader(&mut self, input: ReaderEnum) -> Result<()> {
    self.base.set_reader(input)
  }

  fn set_reader_test_point(&mut self) -> Result<()> {
    self.base.set_reader_test_point()
  }

  fn get_attribute_source(&self) -> &Attributes {
    self.base.input.get_attribute_source()
  }

  fn get_attribute_source_mut(&mut self) -> &mut Attributes {
    self.base.input.get_attribute_source_mut()
  }
}

impl<T, V> TokenFilter for FilteringTokenFilter<T, V>
where
  T: TokenStream,
  V: FilteringTokenFilterBase,
{
}

/// Base trait for token filters that may remove tokens.
/// Implement [`accept`](FilteringTokenFilterBase::accept) and return a `bool` indicating whether the current token should be preserved.
/// [`increment_token`](TokenStream::increment_token) uses this method to decide if a token should be passed to the caller.
pub trait FilteringTokenFilterBase {
  /// Returns whether the current input token should be emitted by `increment_token`.
  fn accept(&self, att: &Attributes) -> Result<bool>;
}
