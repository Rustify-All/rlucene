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
use crate::core::analysis::token_filter::{TokenFilter, TokenFilterBase};
use crate::core::analysis::token_stream::TokenStream;
use crate::core::util::attribute_source::{AttributeSource, Attributes};
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

impl<T, V> TokenStream for FilteringTokenFilter<T, V>
where
    T: TokenStream<AttributeSource = Attributes>,
    V: FilteringTokenFilterBase,
{
    fn increment_token(&mut self) -> Result<bool> {
        self.skipped_positions = 0;
        {
            let att = self.base.input.get_attribute_source_mut();
            if att.get_position_increment().is_none() {
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
            if self.sub.accept(att) {
                if self.skipped_positions != 0 {
                    let new_pos = att.get_position_increment().unwrap() + self.skipped_positions;
                    att.set_position_increment(new_pos)?;
                }
                return Ok(true);
            }
            self.skipped_positions += att.get_position_increment().unwrap();
        }
        Ok(false)
    }

    fn end(&mut self) -> Result<()> {
        self.base.end()?;
        let att = self.base.input.get_attribute_source_mut();
        // we can safely unwrap
        att.set_position_increment(att.get_position_increment().unwrap() + self.skipped_positions)?;
        Ok(())
    }

    fn reset(&mut self) -> Result<()> {
        self.base.reset()?;
        self.skipped_positions = 0;
        Ok(())
    }

    fn close(&mut self) -> Result<()> {
        self.base.close()
    }

    type AttributeSource = Attributes;

    fn get_attribute_source(&self) -> &Self::AttributeSource {
        self.base.input.get_attribute_source()
    }

    fn get_attribute_source_mut(&mut self) -> &mut Self::AttributeSource {
        self.base.input.get_attribute_source_mut()
    }
}

impl<T, V> TokenFilter for FilteringTokenFilter<T, V>
where
    T: TokenStream<AttributeSource = Attributes>,
    V: FilteringTokenFilterBase,
{
}

/// Abstract base class for `TokenFilter`s that may remove tokens.
/// You must implement [`accept`](FilteringTokenFilterBase::accept) and return a boolean indicating whether the current token should be preserved.
/// [`increment_token`](TokenStream::increment_token) uses this method to decide if a token should be passed to the caller.
pub trait FilteringTokenFilterBase {
    /// Override this method and return if the current input token should be returned by #incrementToken.
    fn accept(&self, att: &Attributes) -> bool;
}
