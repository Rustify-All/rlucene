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
use crate::core::analysis::token_stream::TokenStream;
use crate::core::util::attribute_source::Attributes;
use crate::core::util::error::lucene_error::Result;
/// A `TokenFilter` is a `TokenStream` whose input is another `TokenStream`.
/// See also: [`TokenStream`].
pub trait TokenFilter: TokenStream {}

pub struct TokenFilterBase<T>
where
    T: TokenStream,
{
    pub(crate) input: T,
}
impl<T> TokenFilterBase<T>
where
    T: TokenStream,
{
    pub(crate) fn new(input: T) -> Self {
        TokenFilterBase { input }
    }
}

impl<T> Drop for TokenFilterBase<T>
where
    T: TokenStream,
{
    fn drop(&mut self) {
        self.close().expect("should not fail");
    }
}

impl<T> TokenStream for TokenFilterBase<T>
where
    T: TokenStream,
{
    fn end(&mut self) -> Result<()> {
        self.input.end()
    }

    fn reset(&mut self) -> Result<()> {
        self.input.reset()
    }

    fn close(&mut self) -> Result<()> {
        self.input.close()
    }

    type AttributeSource = Attributes;

    fn get_attribute_source(&self) -> &Self::AttributeSource {
        unreachable!("should not be called")
    }

    fn get_attribute_source_mut(&mut self) -> &mut Self::AttributeSource {
        unreachable!("should not be called")
    }
}
