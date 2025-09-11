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
use crate::core::analysis::token_attributes::char_term_attribute::CharTermAttribute;
use crate::core::analysis::token_attributes::offset_attribute::OffsetAttribute;
use crate::core::analysis::token_attributes::packed_token_attribute_impl::PackedTokenAttributeImpl;
use crate::core::analysis::token_attributes::position_increment_attribute::PositionIncrementAttribute;
use crate::core::analysis::token_attributes::term_frequency_attribute::TermFrequencyAttribute;
use crate::core::analysis::token_attributes::term_to_bytes_ref_attribute::TermToBytesRefAttribute;
use crate::core::index::BytesRef;
use crate::core::util::attribute::Attribute;
use crate::core::util::attribute_impl::AttributeImpl;
use std::borrow::Cow;

use crate::core::util::error::lucene_error::{LuceneError, Result};
pub trait AttributeSource {
    // OffsetAttribute
    fn start_offset(&self) -> Option<i32> {
        None
    }

    fn end_offset(&self) -> Option<i32> {
        None
    }

    // PositionIncrementAttribute
    fn get_position_increment(&self) -> Option<i32> {
        None
    }
    fn set_position_increment(&mut self, _position_increment: i32) -> Result<()> {
        Err(LuceneError::unsupported_operation(""))
    }

    // PayloadAttribute;
    fn get_payload(&self) -> Option<&BytesRef<Vec<u8>>> {
        None
    }

    // TermToBytesRefAttribute;
    fn get_bytes_ref(&mut self) -> Option<Cow<'_, BytesRef<Vec<u8>>>> {
        None
    }

    // TermFrequencyAttribute;
    fn get_term_frequency(&self) -> Option<i32> {
        None
    }

    fn end_attributes(&mut self);
}

pub enum Attributes {
    PackedToken(PackedTokenAttributeImpl),
}

impl Attribute for Attributes {}

impl CharTermAttribute for Attributes {
    fn length(&self) -> usize {
        match self {
            Attributes::PackedToken(attr) => attr.length(),
        }
    }

    fn copy_buffer(&mut self, buffer: &[char], offset: usize, length: usize) {
        match self {
            Attributes::PackedToken(attr) => attr.copy_buffer(buffer, offset, length),
        }
    }

    fn buffer_mut(&mut self) -> &mut [char] {
        match self {
            Attributes::PackedToken(attr) => attr.buffer_mut(),
        }
    }

    fn buffer(&self) -> &[char] {
        match self {
            Attributes::PackedToken(attr) => attr.buffer(),
        }
    }

    fn resize_buffer(&mut self, new_size: usize) -> &mut [char] {
        match self {
            Attributes::PackedToken(attr) => attr.resize_buffer(new_size),
        }
    }

    fn set_length(&mut self, length: usize) -> Result<&mut Self> {
        match self {
            Attributes::PackedToken(attr) => {
                attr.set_length(length)?;
                Ok(self)
            },
        }
    }

    fn set_empty(&mut self) -> &mut Self {
        match self {
            Attributes::PackedToken(attr) => {
                attr.set_empty();
                self
            },
        }
    }

    fn append_range(&mut self, csq: &str, start: usize, end: usize) -> &mut Self {
        match self {
            Attributes::PackedToken(attr) => {
                attr.append_range(csq, start, end);
                self
            },
        }
    }

    fn append_char(&mut self, c: char) -> &mut Self {
        match self {
            Attributes::PackedToken(attr) => {
                attr.append_char(c);
                self
            },
        }
    }

    fn append_str(&mut self, s: Option<&str>) -> &mut Self {
        match self {
            Attributes::PackedToken(attr) => {
                attr.append_str(s);
                self
            },
        }
    }

    fn append_term_attribute<C>(&mut self, term_att: Option<&mut C>) -> &mut Self
    where
        C: CharTermAttribute,
    {
        match self {
            Attributes::PackedToken(attr) => {
                attr.append_term_attribute(term_att);
                self
            },
        }
    }
}

impl Default for Attributes {
    fn default() -> Self {
        Attributes::PackedToken(PackedTokenAttributeImpl::default())
    }
}

impl AttributeSource for Attributes {
    fn start_offset(&self) -> Option<i32> {
        match self {
            Attributes::PackedToken(attr) => Some(attr.start_offset()),
        }
    }

    fn end_offset(&self) -> Option<i32> {
        match self {
            Attributes::PackedToken(attr) => Some(attr.end_offset()),
        }
    }

    fn get_position_increment(&self) -> Option<i32> {
        match self {
            Attributes::PackedToken(attr) => Some(attr.get_position_increment()),
        }
    }

    fn set_position_increment(&mut self, position_increment: i32) -> Result<()> {
        match self {
            Attributes::PackedToken(attr) => attr.set_position_increment(position_increment),
        }
    }

    fn get_payload(&self) -> Option<&BytesRef<Vec<u8>>> {
        match self {
            Attributes::PackedToken(_) => None,
        }
    }

    fn get_bytes_ref(&mut self) -> Option<Cow<'_, BytesRef<Vec<u8>>>> {
        match self {
            Attributes::PackedToken(attr) => Some(attr.base.get_bytes_ref()),
        }
    }

    fn get_term_frequency(&self) -> Option<i32> {
        match self {
            Attributes::PackedToken(attr) => Some(attr.get_term_frequency()),
        }
    }

    fn end_attributes(&mut self) {
        match self {
            Attributes::PackedToken(attr) => attr.end(),
        }
    }
}

impl OffsetAttribute for Attributes {
    fn start_offset(&self) -> i32 {
        match self {
            Attributes::PackedToken(attr) => attr.start_offset(),
        }
    }

    fn set_offset(&mut self, start_offset: i32, end_offset: i32) -> Result<()> {
        match self {
            Attributes::PackedToken(attr) => attr.set_offset(start_offset, end_offset),
        }
    }

    fn end_offset(&self) -> i32 {
        match self {
            Attributes::PackedToken(attr) => attr.end_offset(),
        }
    }
}

pub struct EmptyAttributeSource;

impl Default for EmptyAttributeSource {
    fn default() -> Self {
        EmptyAttributeSource
    }
}

impl AttributeSource for EmptyAttributeSource {
    fn end_attributes(&mut self) {}
}

// AttributeSource
pub enum Either2AttributeSource<A, B> {
    A(A),
    B(B),
}

impl<A, B> AttributeSource for Either2AttributeSource<A, B>
where
    A: AttributeSource,
    B: AttributeSource,
{
    fn start_offset(&self) -> Option<i32> {
        match self {
            Either2AttributeSource::A(t) => t.start_offset(),
            Either2AttributeSource::B(s) => s.start_offset(),
        }
    }

    fn end_offset(&self) -> Option<i32> {
        match self {
            Either2AttributeSource::A(t) => t.end_offset(),
            Either2AttributeSource::B(s) => s.end_offset(),
        }
    }

    fn get_position_increment(&self) -> Option<i32> {
        match self {
            Either2AttributeSource::A(t) => t.get_position_increment(),
            Either2AttributeSource::B(s) => s.get_position_increment(),
        }
    }

    fn set_position_increment(&mut self, _position_increment: i32) -> Result<()> {
        match self {
            Either2AttributeSource::A(t) => t.set_position_increment(_position_increment),
            Either2AttributeSource::B(s) => s.set_position_increment(_position_increment),
        }
    }

    fn get_payload(&self) -> Option<&BytesRef<Vec<u8>>> {
        match self {
            Either2AttributeSource::A(t) => t.get_payload(),
            Either2AttributeSource::B(s) => s.get_payload(),
        }
    }

    fn get_bytes_ref(&mut self) -> Option<Cow<'_, BytesRef<Vec<u8>>>> {
        match self {
            Either2AttributeSource::A(t) => t.get_bytes_ref(),
            Either2AttributeSource::B(s) => s.get_bytes_ref(),
        }
    }

    fn get_term_frequency(&self) -> Option<i32> {
        match self {
            Either2AttributeSource::A(t) => t.get_term_frequency(),
            Either2AttributeSource::B(s) => s.get_term_frequency(),
        }
    }

    fn end_attributes(&mut self) {
        match self {
            Either2AttributeSource::A(t) => t.end_attributes(),
            Either2AttributeSource::B(s) => s.end_attributes(),
        }
    }
}
