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
#[cfg(test)]
use crate::core::analysis::char_filter::CharFilter;
#[cfg(test)]
use crate::core::analysis::char_filter::tests::{CharFilter1, CharFilter2};

use crate::core::analysis::reusable_string_reader::ReusableStringReader;
use crate::core::analysis::tokenizer::IllegalStateReader;
use crate::core::util::error::lucene_error::Result;
pub trait Reader {
    /// Reads a single character. Returns -1 on EOF
    fn read(&mut self) -> Result<i32> {
        let mut cb: Vec<char> = vec![char::from(0); 1];
        if self.read_range(&mut cb, 0, 1)? == -1 {
            return Ok(-1);
        }
        Ok(cb[0] as i32)
    }
    /// Reads characters into the buffer, starting at `off`,
    /// up to `len` characters. Returns the number of chars read,
    /// or -1 on EOF.
    fn read_range(&mut self, buf: &mut [char], off: usize, len: usize) -> Result<i32>;
    fn close(&mut self) -> Result<()>;
}

#[derive(Debug, Clone)]
pub enum ReaderEnum {
    ReusedString(ReusableStringReader),
    IllegalState(IllegalStateReader),
    #[cfg(test)]
    CharFilter1(CharFilter1),
    #[cfg(test)]
    CharFilter2(CharFilter2),
}
// for std::mem::take
impl Default for ReaderEnum {
    fn default() -> Self {
        ReaderEnum::IllegalState(IllegalStateReader)
    }
}
impl ReaderEnum {
    pub fn correct_offset(&self, corrected: i32) -> i32 {
        match self {
            #[cfg(test)]
            ReaderEnum::CharFilter1(r) => r.correct_offset(corrected),
            #[cfg(test)]
            ReaderEnum::CharFilter2(r) => r.correct_offset(corrected),
            // not a CharFilter
            _ => corrected,
        }
    }
}
impl Reader for ReaderEnum {
    fn read(&mut self) -> Result<i32> {
        match self {
            ReaderEnum::ReusedString(r) => r.read(),
            ReaderEnum::IllegalState(r) => r.read(),
            #[cfg(test)]
            ReaderEnum::CharFilter1(r) => r.read(),
            #[cfg(test)]
            ReaderEnum::CharFilter2(r) => r.read(),
        }
    }

    fn read_range(&mut self, buf: &mut [char], off: usize, len: usize) -> Result<i32> {
        match self {
            ReaderEnum::ReusedString(r) => r.read_range(buf, off, len),
            ReaderEnum::IllegalState(r) => r.read_range(buf, off, len),
            #[cfg(test)]
            ReaderEnum::CharFilter1(r) => r.read_range(buf, off, len),
            #[cfg(test)]
            ReaderEnum::CharFilter2(r) => r.read_range(buf, off, len),
        }
    }

    fn close(&mut self) -> Result<()> {
        match self {
            ReaderEnum::ReusedString(r) => r.close(),
            ReaderEnum::IllegalState(r) => r.close(),
            #[cfg(test)]
            ReaderEnum::CharFilter1(r) => CharFilter::close(r),
            #[cfg(test)]
            ReaderEnum::CharFilter2(r) => CharFilter::close(r),
        }
    }
}

impl<'a> From<&'a str> for ReaderEnum {
    fn from(text: &'a str) -> Self {
        let mut reader = ReusableStringReader::new();
        reader.set_value(text);
        ReaderEnum::ReusedString(reader)
    }
}

impl From<&String> for ReaderEnum {
    fn from(text: &String) -> Self {
        ReaderEnum::from(text.as_str())
    }
}

impl From<String> for ReaderEnum {
    fn from(text: String) -> Self {
        let mut reader = ReusableStringReader::new();
        reader.set_value(&text);
        ReaderEnum::ReusedString(reader)
    }
}
