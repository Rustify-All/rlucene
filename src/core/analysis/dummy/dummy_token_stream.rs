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
use crate::core::util::dummy::dummy_attribute_source::DummyAttributeSource;

#[derive(Debug)]
pub struct DummyTokenStream;

impl Drop for DummyTokenStream {
    fn drop(&mut self) {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }
}

impl TokenStream for DummyTokenStream {
    fn increment_token(&mut self) -> crate::core::util::error::lucene_error::Result<bool> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn end(&mut self) -> crate::core::util::error::lucene_error::Result<()> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn default_end(&mut self) -> crate::core::util::error::lucene_error::Result<()> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn reset(&mut self) -> crate::core::util::error::lucene_error::Result<()> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn default_reset(&mut self) -> crate::core::util::error::lucene_error::Result<()> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn close(&mut self) -> crate::core::util::error::lucene_error::Result<()> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    type AttributeSource = DummyAttributeSource;

    fn get_attribute_source(&self) -> &Self::AttributeSource {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn get_attribute_source_mut(&mut self) -> &mut Self::AttributeSource {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }
}
