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
use crate::core::analysis::token_attributes::char_term_attribute::CharTermAttribute;
use crate::core::analysis::token_attributes::offset_attribute::OffsetAttribute;
use crate::core::analysis::token_stream::TokenStream;
use crate::core::util::attribute_source::Attributes;
use crate::core::util::error::lucene_error::Result;

pub trait Analyzer {
    fn create_components(&self);

    fn get_position_increment_gap(&self, _field_name: &str) -> i32 {
        0
    }
    fn get_offset_gap(&self, _field_name: &str) -> i32 {
        1
    }
}

pub struct TokenStreamComponents<TS>
where
    TS: TokenStream,
{
    sink: Option<TS>,
}
impl<TS> TokenStreamComponents<TS>
where
    TS: TokenStream,
{
    pub fn new(sink: TS) -> Self {
        Self { sink: Some(sink) }
    }
    fn set_reader(&mut self, reader: ReaderEnum) -> Result<()> {
        self.sink.as_mut().unwrap().set_reader(reader)
    }
    pub fn get_token_stream(&mut self) -> &mut TS {
        self.sink.as_mut().unwrap()
    }
    pub fn take_token_stream(&mut self) -> Option<TS> {
        self.sink.take()
    }
}

struct StringTokenStream {
    value: String,
    length: i32,
    used: bool,
    att: Attributes,
}
impl StringTokenStream {
    fn new(att: Attributes, value: &str, length: i32) -> Self {
        Self {
            value: value.to_string(),
            length,
            used: true,
            att,
        }
    }
}
impl TokenStream for StringTokenStream {
    fn increment_token(&mut self) -> Result<bool> {
        if self.used {
            return Ok(true);
        }
        // self.clear_attributes();
        self.att.append_str(Some(&self.value));
        self.att.set_offset(0, self.length)?;
        self.used = true;
        Ok(true)
    }

    fn end(&mut self) -> Result<()> {
        self.default_end()?;
        self.att.set_offset(self.length, self.length)
    }

    fn reset(&mut self) -> Result<()> {
        self.used = false;
        Ok(())
    }

    type AttributeSource = Attributes;

    fn get_attribute_source(&self) -> &Self::AttributeSource {
        &self.att
    }

    fn get_attribute_source_mut(&mut self) -> &mut Self::AttributeSource {
        &mut self.att
    }
}
