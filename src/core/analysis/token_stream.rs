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
use crate::core::analysis::token_attributes::packed_token_attribute_impl::PackedTokenAttributeImpl;
use crate::core::util::attribute_source::{AttributeSource, Attributes};
use crate::core::util::error::lucene_error::Result;
pub trait TokenStream {
    fn increment_token(&mut self) -> Result<bool> {
        unreachable!("must be implemented by sub");
    }
    fn end(&mut self) -> Result<()>;
    fn default_end(&mut self) -> Result<()> {
        self.get_attribute_source_mut().end_attributes();
        Ok(())
    }
    fn reset(&mut self) -> Result<()> {
        Ok(())
    }
    fn default_reset(&mut self) -> Result<()> {
        Ok(())
    }
    fn close(&mut self) -> Result<()> {
        Ok(())
    }
    type AttributeSource: AttributeSource;
    fn get_attribute_source(&self) -> &Self::AttributeSource;
    fn get_attribute_source_mut(&mut self) -> &mut Self::AttributeSource;
    fn set_reader(&mut self, _input: ReaderEnum) -> Result<()> {
        Ok(())
    }
    fn set_reader_test_point(&mut self) {}
}
pub fn default_attribute() -> Attributes {
    Attributes::PackedToken(PackedTokenAttributeImpl::new())
}
