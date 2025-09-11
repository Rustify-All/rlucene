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
use crate::core::analysis::reader::{Reader, ReaderEnum};
use crate::core::analysis::token_stream::TokenStream;
use crate::core::util::attribute_source::Attributes;
use crate::core::util::error::lucene_error::{LuceneError, Result};
/// A `Tokenizer` is a `TokenStream` whose input is a `Reader`.
pub trait Tokenizer: TokenStream {
    fn get_tokenizer_base_mut(&mut self) -> &mut TokenizerBase;
    fn get_tokenizer_base(&self) -> &TokenizerBase;
    /// Return the corrected offset.
    /// If input is a CharFilter this method calls CharFilter.correctOffset else returns currentOff.
    fn correct_offset(&self, current_off: i32) -> i32 {
        let base = self.get_tokenizer_base();
        base.input.correct_offset(current_off)
    }
}

pub struct TokenizerBase {
    /// The text source for this Tokenizer.
    pub(crate) input: ReaderEnum,
    /// Pending reader: not actually assigned to input until reset()
    pub(crate) input_pending: ReaderEnum,
}
impl Default for TokenizerBase {
    fn default() -> Self {
        Self::new()
    }
}

impl TokenizerBase {
    pub fn new() -> Self {
        Self {
            input_pending: ReaderEnum::IllegalState(IllegalStateReader),
            input: ReaderEnum::IllegalState(IllegalStateReader),
        }
    }
    pub fn set_reader(&mut self, input: ReaderEnum) {
        self.input_pending = input;
    }
}
impl TokenStream for TokenizerBase {
    fn end(&mut self) -> Result<()> {
        self.default_end()
    }

    fn reset(&mut self) -> Result<()> {
        self.default_reset()?;
        self.input = std::mem::take(&mut self.input_pending);
        self.input_pending = ReaderEnum::IllegalState(IllegalStateReader);
        Ok(())
    }

    /// Releases resources associated with this stream.
    fn close(&mut self) -> Result<()> {
        self.input.close()?;
        self.input = ReaderEnum::IllegalState(IllegalStateReader);
        self.input_pending = ReaderEnum::IllegalState(IllegalStateReader);
        Ok(())
    }

    type AttributeSource = Attributes;

    fn get_attribute_source(&self) -> &Self::AttributeSource {
        unreachable!("should not be called")
    }

    fn get_attribute_source_mut(&mut self) -> &mut Self::AttributeSource {
        unreachable!("should not be called")
    }

    fn set_reader(&mut self, input: ReaderEnum) -> Result<()> {
        if !matches!(self.input, ReaderEnum::IllegalState(_)) {
            return Err(LuceneError::illegal_state(
                "TokenStream contract violation: close() call missing",
            ));
        }
        self.input_pending = input;
        self.set_reader_test_point();
        Ok(())
    }
}
#[derive(Debug, Clone, Default)]
pub struct IllegalStateReader;
impl Reader for IllegalStateReader {
    fn read_range(&mut self, _buf: &mut [char], _off: usize, _len: usize) -> Result<i32> {
        Err(LuceneError::illegal_state(
            "TokenStream contract violation: reset()/close() call missing, \
reset() called multiple times, or subclass does not call super.reset(). \
Please see Javadocs of TokenStream class for more information about the correct consuming workflow.",
        ))
    }

    fn close(&mut self) -> Result<()> {
        Ok(())
    }
}
