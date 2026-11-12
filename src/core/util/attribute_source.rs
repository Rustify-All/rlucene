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
use crate::core::analysis::token_attributes::bytes_term_attribute_impl::BytesTermAttributeImpl;
use crate::core::analysis::token_attributes::char_term_attribute::CharTermAttribute;
use crate::core::analysis::token_attributes::char_term_attribute_impl::CharTermAttributeImpl;
use crate::core::analysis::token_attributes::packed_token_and_binary::BinaryTokenStreamAttributeImpl;
use crate::core::analysis::token_attributes::packed_token_attribute_impl::PackedTokenAttributeImpl;
use crate::core::index::BytesRef;
use crate::core::util::attribute::Attribute;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::impl_from_for_enum;
#[cfg(test)]
use crate::test_framework::core::analysis::base_token_stream_test_case::CheckClearAttributesAttribute;
#[cfg(test)]
use crate::test_framework::core::index::term_vectors::RandomTokenStreamAttr;
#[cfg(any(test, debug_assertions))]
use std::collections::HashSet;
use std::fmt::{Display, Formatter};

pub trait AttributeSource {
  /// True only when this type guarantees that no term-frequency attribute exists.
  /// The default preserves getter calls, including those of custom sources.
  const TERM_FREQUENCY_IS_ABSENT: bool = false;

  // CharTermAttribute
  fn length(&self) -> Result<usize> {
    Err(LuceneError::unsupported_operation(""))
  }
  fn copy_buffer(&mut self, _buffer: &[char], _offset: usize, _length: usize) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }
  fn buffer_mut(&mut self) -> Result<&mut [char]> {
    Err(LuceneError::unsupported_operation(""))
  }
  fn buffer(&self) -> Result<&[char]> {
    Err(LuceneError::unsupported_operation(""))
  }
  fn resize_buffer(&mut self, _new_size: usize) -> Result<&mut [char]> {
    Err(LuceneError::unsupported_operation(""))
  }
  fn set_length(&mut self, _length: usize) -> Result<&mut Self> {
    Err(LuceneError::unsupported_operation(""))
  }
  fn set_empty(&mut self) -> Result<&mut Self> {
    Err(LuceneError::unsupported_operation(""))
  }
  fn append_range(&mut self, _csq: Option<&str>, _start: usize, _end: usize) -> Result<&mut Self> {
    Err(LuceneError::unsupported_operation(""))
  }
  fn append_char(&mut self, _c: char) -> Result<&mut Self> {
    Err(LuceneError::unsupported_operation(""))
  }
  fn append_str(&mut self, _s: Option<&str>) -> Result<&mut Self> {
    Err(LuceneError::unsupported_operation(""))
  }
  fn append_term_attribute<C>(&mut self, _term_att: Option<&mut C>) -> Result<&mut Self>
  where
    C: CharTermAttribute,
  {
    Err(LuceneError::unsupported_operation(""))
  }

  // OffsetAttribute
  fn start_offset(&self) -> Result<i32> {
    Err(LuceneError::unsupported_operation(""))
  }
  fn set_offset(&mut self, _start_offset: i32, _end_offset: i32) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }
  fn end_offset(&self) -> Result<i32> {
    Err(LuceneError::unsupported_operation(""))
  }

  // BytesTermAttribute
  fn set_bytes_ref(&mut self, _bytes: Option<BytesRef<Vec<u8>>>) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }

  // PositionIncrementAttribute
  fn get_position_increment(&self) -> Result<i32> {
    Err(LuceneError::unsupported_operation(""))
  }
  fn set_position_increment(&mut self, _position_increment: i32) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }

  // PayloadAttribute;
  fn get_payload(&self) -> Result<Option<&BytesRef<Vec<u8>>>> {
    Ok(None)
  }
  fn set_payload(&mut self, _payload: Option<BytesRef<Vec<u8>>>) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }

  // TermToBytesRefAttribute;
  fn get_bytes_ref(&mut self) -> Result<Option<&BytesRef<Vec<u8>>>> {
    Ok(None)
  }

  // TermFrequencyAttribute;
  fn set_term_frequency(&mut self, _term_frequency: i32) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }
  fn get_term_frequency(&self) -> Result<i32> {
    Err(LuceneError::unsupported_operation(""))
  }

  // PositionLengthAttribute
  fn set_position_length(&mut self, _position_length: i32) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }
  fn get_position_length(&self) -> Result<i32> {
    Err(LuceneError::unsupported_operation(""))
  }
  // KeywordAttribute
  fn is_keyword(&self) -> Result<bool> {
    Err(LuceneError::unsupported_operation(""))
  }
  fn set_keyword(&mut self, _is_keyword: bool) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }
  // FlagsAttribute
  fn get_flags(&self) -> Result<i32> {
    Err(LuceneError::unsupported_operation(""))
  }
  fn set_flags(&mut self, _flags: i32) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }
  // BoostAttribute
  fn set_boost(&mut self, _boost: f32) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }
  fn get_boost(&self) -> Result<f32> {
    Err(LuceneError::unsupported_operation(""))
  }
  // MaxNonCompetitiveBoostAttribute
  fn set_max_non_competitive_boost(&mut self, _max_non_competitive_boost: f32) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }
  fn get_max_non_competitive_boost(&self) -> Result<f32> {
    Err(LuceneError::unsupported_operation(""))
  }
  fn set_competitive_term(&mut self, _competitive_term: Option<BytesRef<Vec<u8>>>) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }
  fn get_competitive_term(&self) -> Result<Option<&BytesRef<Vec<u8>>>> {
    Err(LuceneError::unsupported_operation(""))
  }
  // TypeAttribute;
  fn type_(&self) -> Result<&str> {
    Err(LuceneError::unsupported_operation(""))
  }
  fn set_type(&mut self, _type_: &str) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn end_attributes(&mut self) {}

  fn clear_attributes(&mut self) -> Result<()>;
}

impl<T> AttributeSource for &T
where
  T: AttributeSource,
{
  const TERM_FREQUENCY_IS_ABSENT: bool = T::TERM_FREQUENCY_IS_ABSENT;

  fn length(&self) -> Result<usize> {
    (**self).length()
  }

  fn buffer(&self) -> Result<&[char]> {
    (**self).buffer()
  }

  fn start_offset(&self) -> Result<i32> {
    (**self).start_offset()
  }

  fn end_offset(&self) -> Result<i32> {
    (**self).end_offset()
  }

  fn get_position_increment(&self) -> Result<i32> {
    (**self).get_position_increment()
  }

  fn get_payload(&self) -> Result<Option<&BytesRef<Vec<u8>>>> {
    (**self).get_payload()
  }

  fn get_term_frequency(&self) -> Result<i32> {
    (**self).get_term_frequency()
  }

  fn get_position_length(&self) -> Result<i32> {
    (**self).get_position_length()
  }

  fn is_keyword(&self) -> Result<bool> {
    (**self).is_keyword()
  }

  fn get_flags(&self) -> Result<i32> {
    (**self).get_flags()
  }

  fn get_boost(&self) -> Result<f32> {
    (**self).get_boost()
  }

  fn get_max_non_competitive_boost(&self) -> Result<f32> {
    (**self).get_max_non_competitive_boost()
  }

  fn get_competitive_term(&self) -> Result<Option<&BytesRef<Vec<u8>>>> {
    (**self).get_competitive_term()
  }

  fn type_(&self) -> Result<&str> {
    (**self).type_()
  }

  fn clear_attributes(&mut self) -> Result<()> {
    Err(LuceneError::unsupported_operation(
      "cannot clear attributes through an immutable reference",
    ))
  }
}

impl<T> AttributeSource for &mut T
where
  T: AttributeSource,
{
  const TERM_FREQUENCY_IS_ABSENT: bool = T::TERM_FREQUENCY_IS_ABSENT;

  fn length(&self) -> Result<usize> {
    (**self).length()
  }

  fn copy_buffer(&mut self, buffer: &[char], offset: usize, length: usize) -> Result<()> {
    (**self).copy_buffer(buffer, offset, length)
  }

  fn buffer_mut(&mut self) -> Result<&mut [char]> {
    (**self).buffer_mut()
  }

  fn buffer(&self) -> Result<&[char]> {
    (**self).buffer()
  }

  fn resize_buffer(&mut self, new_size: usize) -> Result<&mut [char]> {
    (**self).resize_buffer(new_size)
  }

  fn set_length(&mut self, length: usize) -> Result<&mut Self> {
    (**self).set_length(length)?;
    Ok(self)
  }

  fn set_empty(&mut self) -> Result<&mut Self> {
    (**self).set_empty()?;
    Ok(self)
  }

  fn append_range(&mut self, csq: Option<&str>, start: usize, end: usize) -> Result<&mut Self> {
    (**self).append_range(csq, start, end)?;
    Ok(self)
  }

  fn append_char(&mut self, c: char) -> Result<&mut Self> {
    (**self).append_char(c)?;
    Ok(self)
  }

  fn append_str(&mut self, s: Option<&str>) -> Result<&mut Self> {
    (**self).append_str(s)?;
    Ok(self)
  }

  fn append_term_attribute<C>(&mut self, term_att: Option<&mut C>) -> Result<&mut Self>
  where
    C: CharTermAttribute,
  {
    (**self).append_term_attribute(term_att)?;
    Ok(self)
  }

  fn start_offset(&self) -> Result<i32> {
    (**self).start_offset()
  }

  fn set_offset(&mut self, start_offset: i32, end_offset: i32) -> Result<()> {
    (**self).set_offset(start_offset, end_offset)
  }

  fn end_offset(&self) -> Result<i32> {
    (**self).end_offset()
  }

  fn set_bytes_ref(&mut self, bytes: Option<BytesRef<Vec<u8>>>) -> Result<()> {
    (**self).set_bytes_ref(bytes)
  }

  fn get_position_increment(&self) -> Result<i32> {
    (**self).get_position_increment()
  }

  fn set_position_increment(&mut self, position_increment: i32) -> Result<()> {
    (**self).set_position_increment(position_increment)
  }

  fn get_payload(&self) -> Result<Option<&BytesRef<Vec<u8>>>> {
    (**self).get_payload()
  }

  fn set_payload(&mut self, payload: Option<BytesRef<Vec<u8>>>) -> Result<()> {
    (**self).set_payload(payload)
  }

  fn get_bytes_ref(&mut self) -> Result<Option<&BytesRef<Vec<u8>>>> {
    (**self).get_bytes_ref()
  }

  fn get_term_frequency(&self) -> Result<i32> {
    (**self).get_term_frequency()
  }

  fn set_term_frequency(&mut self, term_frequency: i32) -> Result<()> {
    (**self).set_term_frequency(term_frequency)
  }

  fn set_position_length(&mut self, position_length: i32) -> Result<()> {
    (**self).set_position_length(position_length)
  }

  fn get_position_length(&self) -> Result<i32> {
    (**self).get_position_length()
  }

  fn is_keyword(&self) -> Result<bool> {
    (**self).is_keyword()
  }

  fn set_keyword(&mut self, is_keyword: bool) -> Result<()> {
    (**self).set_keyword(is_keyword)
  }

  fn get_flags(&self) -> Result<i32> {
    (**self).get_flags()
  }

  fn set_flags(&mut self, flags: i32) -> Result<()> {
    (**self).set_flags(flags)
  }

  fn set_boost(&mut self, boost: f32) -> Result<()> {
    (**self).set_boost(boost)
  }

  fn get_boost(&self) -> Result<f32> {
    (**self).get_boost()
  }

  fn set_max_non_competitive_boost(&mut self, max_non_competitive_boost: f32) -> Result<()> {
    (**self).set_max_non_competitive_boost(max_non_competitive_boost)
  }

  fn get_max_non_competitive_boost(&self) -> Result<f32> {
    (**self).get_max_non_competitive_boost()
  }

  fn set_competitive_term(&mut self, competitive_term: Option<BytesRef<Vec<u8>>>) -> Result<()> {
    (**self).set_competitive_term(competitive_term)
  }

  fn get_competitive_term(&self) -> Result<Option<&BytesRef<Vec<u8>>>> {
    (**self).get_competitive_term()
  }

  fn type_(&self) -> Result<&str> {
    (**self).type_()
  }

  fn set_type(&mut self, type_: &str) -> Result<()> {
    (**self).set_type(type_)
  }

  fn end_attributes(&mut self) {
    (**self).end_attributes()
  }

  fn clear_attributes(&mut self) -> Result<()> {
    (**self).clear_attributes()
  }
}

pub enum Attributes {
  PackedToken(CharTermAttributeImpl<PackedTokenAttributeImpl>),
  BytesTerm(BytesTermAttributeImpl),
  BinaryTokenStream(BinaryTokenStreamAttributeImpl),
  #[cfg(test)]
  RandomTokenStream(RandomTokenStreamAttr),
}
impl Display for Attributes {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    match self {
      Attributes::PackedToken(attr) => attr.fmt(f),
      Attributes::BytesTerm(attr) => attr.fmt(f),
      Attributes::BinaryTokenStream(attr) => attr.fmt(f),
      #[cfg(test)]
      Attributes::RandomTokenStream(attr) => attr.fmt(f),
    }
  }
}
#[cfg(test)]
impl Attributes {
  pub fn get_and_reset_clear_called(&mut self) -> Result<bool> {
    match self {
      Attributes::PackedToken(attr) => Ok(attr.get_and_reset_clear_called()),
      Attributes::BytesTerm(attr) => Ok(attr.get_and_reset_clear_called()),
      Attributes::BinaryTokenStream(attr) => Ok(attr.get_and_reset_clear_called()),
      Attributes::RandomTokenStream(_) => Err(LuceneError::unsupported_operation("")),
    }
  }
}
impl_from_for_enum!(
    Attributes,
    CharTermAttributeImpl<PackedTokenAttributeImpl> => PackedToken,
    BytesTermAttributeImpl=> BytesTerm,
    BinaryTokenStreamAttributeImpl=> BinaryTokenStream,
);
#[cfg(test)]
impl From<RandomTokenStreamAttr> for Attributes {
  fn from(v: RandomTokenStreamAttr) -> Self {
    Attributes::RandomTokenStream(v)
  }
}
impl Attribute for Attributes {
  #[cfg(any(test, debug_assertions))]
  fn get_attribute_name(&self) -> Result<&HashSet<String>> {
    match self {
      Attributes::PackedToken(attr) => attr.get_attribute_name(),
      Attributes::BytesTerm(attr) => attr.get_attribute_name(),
      Attributes::BinaryTokenStream(attr) => attr.get_attribute_name(),
      #[cfg(test)]
      Attributes::RandomTokenStream(attr) => attr.get_attribute_name(),
    }
  }
}

impl Default for Attributes {
  fn default() -> Self {
    Attributes::PackedToken(expect_invariant!(
      PackedTokenAttributeImpl::new(),
      "the default packed token attribute uses fixed built-in attribute definitions",
    ))
  }
}

impl AttributeSource for Attributes {
  fn length(&self) -> Result<usize> {
    match self {
      Attributes::PackedToken(attr) => AttributeSource::length(attr),
      Attributes::BytesTerm(attr) => AttributeSource::length(attr),
      Attributes::BinaryTokenStream(attr) => AttributeSource::length(attr),
      #[cfg(test)]
      Attributes::RandomTokenStream(attr) => AttributeSource::length(attr),
    }
  }

  fn copy_buffer(&mut self, buffer: &[char], offset: usize, length: usize) -> Result<()> {
    match self {
      Attributes::PackedToken(attr) => AttributeSource::copy_buffer(attr, buffer, offset, length),
      Attributes::BytesTerm(attr) => AttributeSource::copy_buffer(attr, buffer, offset, length),
      Attributes::BinaryTokenStream(attr) => {
        AttributeSource::copy_buffer(attr, buffer, offset, length)
      },
      #[cfg(test)]
      Attributes::RandomTokenStream(attr) => {
        AttributeSource::copy_buffer(attr, buffer, offset, length)
      },
    }
  }

  fn buffer_mut(&mut self) -> Result<&mut [char]> {
    match self {
      Attributes::PackedToken(attr) => AttributeSource::buffer_mut(attr),
      Attributes::BytesTerm(attr) => AttributeSource::buffer_mut(attr),
      Attributes::BinaryTokenStream(attr) => AttributeSource::buffer_mut(attr),
      #[cfg(test)]
      Attributes::RandomTokenStream(attr) => AttributeSource::buffer_mut(attr),
    }
  }

  fn buffer(&self) -> Result<&[char]> {
    match self {
      Attributes::PackedToken(attr) => AttributeSource::buffer(attr),
      Attributes::BytesTerm(attr) => AttributeSource::buffer(attr),
      Attributes::BinaryTokenStream(attr) => AttributeSource::buffer(attr),
      #[cfg(test)]
      Attributes::RandomTokenStream(attr) => AttributeSource::buffer(attr),
    }
  }

  fn resize_buffer(&mut self, new_size: usize) -> Result<&mut [char]> {
    match self {
      Attributes::PackedToken(attr) => AttributeSource::resize_buffer(attr, new_size),
      Attributes::BytesTerm(attr) => AttributeSource::resize_buffer(attr, new_size),
      Attributes::BinaryTokenStream(attr) => AttributeSource::resize_buffer(attr, new_size),
      #[cfg(test)]
      Attributes::RandomTokenStream(attr) => AttributeSource::resize_buffer(attr, new_size),
    }
  }

  fn set_length(&mut self, length: usize) -> Result<&mut Self> {
    match self {
      Attributes::PackedToken(attr) => {
        AttributeSource::set_length(attr, length)?;
        Ok(self)
      },
      Attributes::BytesTerm(attr) => {
        AttributeSource::set_length(attr, length)?;
        Ok(self)
      },
      Attributes::BinaryTokenStream(attr) => {
        AttributeSource::set_length(attr, length)?;
        Ok(self)
      },
      #[cfg(test)]
      Attributes::RandomTokenStream(attr) => {
        AttributeSource::set_length(attr, length)?;
        Ok(self)
      },
    }
  }

  fn set_empty(&mut self) -> Result<&mut Self> {
    match self {
      Attributes::PackedToken(attr) => {
        AttributeSource::set_empty(attr)?;
        Ok(self)
      },
      Attributes::BytesTerm(attr) => {
        AttributeSource::set_empty(attr)?;
        Ok(self)
      },
      Attributes::BinaryTokenStream(attr) => {
        AttributeSource::set_empty(attr)?;
        Ok(self)
      },
      #[cfg(test)]
      Attributes::RandomTokenStream(attr) => {
        AttributeSource::set_empty(attr)?;
        Ok(self)
      },
    }
  }

  fn append_range(&mut self, csq: Option<&str>, start: usize, end: usize) -> Result<&mut Self> {
    match self {
      Attributes::PackedToken(attr) => {
        AttributeSource::append_range(attr, csq, start, end)?;
        Ok(self)
      },
      Attributes::BytesTerm(attr) => {
        AttributeSource::append_range(attr, csq, start, end)?;
        Ok(self)
      },
      Attributes::BinaryTokenStream(attr) => {
        AttributeSource::append_range(attr, csq, start, end)?;
        Ok(self)
      },
      #[cfg(test)]
      Attributes::RandomTokenStream(attr) => {
        AttributeSource::append_range(attr, csq, start, end)?;
        Ok(self)
      },
    }
  }

  fn append_char(&mut self, c: char) -> Result<&mut Self> {
    match self {
      Attributes::PackedToken(attr) => {
        AttributeSource::append_char(attr, c)?;
        Ok(self)
      },
      Attributes::BytesTerm(attr) => {
        AttributeSource::append_char(attr, c)?;
        Ok(self)
      },
      Attributes::BinaryTokenStream(attr) => {
        AttributeSource::append_char(attr, c)?;
        Ok(self)
      },
      #[cfg(test)]
      Attributes::RandomTokenStream(attr) => {
        AttributeSource::append_char(attr, c)?;
        Ok(self)
      },
    }
  }

  fn append_str(&mut self, s: Option<&str>) -> Result<&mut Self> {
    match self {
      Attributes::PackedToken(attr) => {
        AttributeSource::append_str(attr, s)?;
        Ok(self)
      },
      Attributes::BytesTerm(attr) => {
        AttributeSource::append_str(attr, s)?;
        Ok(self)
      },
      Attributes::BinaryTokenStream(attr) => {
        AttributeSource::append_str(attr, s)?;
        Ok(self)
      },
      #[cfg(test)]
      Attributes::RandomTokenStream(attr) => {
        AttributeSource::append_str(attr, s)?;
        Ok(self)
      },
    }
  }

  fn append_term_attribute<C>(&mut self, term_att: Option<&mut C>) -> Result<&mut Self>
  where
    C: CharTermAttribute,
  {
    match self {
      Attributes::PackedToken(attr) => {
        AttributeSource::append_term_attribute(attr, term_att)?;
        Ok(self)
      },
      Attributes::BytesTerm(attr) => {
        AttributeSource::append_term_attribute(attr, term_att)?;
        Ok(self)
      },
      Attributes::BinaryTokenStream(attr) => {
        AttributeSource::append_term_attribute(attr, term_att)?;
        Ok(self)
      },
      #[cfg(test)]
      Attributes::RandomTokenStream(attr) => {
        AttributeSource::append_term_attribute(attr, term_att)?;
        Ok(self)
      },
    }
  }

  fn start_offset(&self) -> Result<i32> {
    match self {
      Attributes::PackedToken(attr) => attr.start_offset(),
      Attributes::BytesTerm(attr) => attr.start_offset(),
      Attributes::BinaryTokenStream(attr) => attr.start_offset(),
      #[cfg(test)]
      Attributes::RandomTokenStream(attr) => attr.start_offset(),
    }
  }

  fn set_offset(&mut self, start_offset: i32, end_offset: i32) -> Result<()> {
    match self {
      Attributes::PackedToken(attr) => attr.set_offset(start_offset, end_offset),
      Attributes::BytesTerm(attr) => attr.set_offset(start_offset, end_offset),
      Attributes::BinaryTokenStream(attr) => attr.set_offset(start_offset, end_offset),
      #[cfg(test)]
      Attributes::RandomTokenStream(attr) => attr.set_offset(start_offset, end_offset),
    }
  }

  fn end_offset(&self) -> Result<i32> {
    match self {
      Attributes::PackedToken(attr) => attr.end_offset(),
      Attributes::BytesTerm(attr) => attr.end_offset(),
      Attributes::BinaryTokenStream(attr) => attr.end_offset(),
      #[cfg(test)]
      Attributes::RandomTokenStream(attr) => attr.end_offset(),
    }
  }

  fn set_bytes_ref(&mut self, bytes: Option<BytesRef<Vec<u8>>>) -> Result<()> {
    match self {
      Attributes::PackedToken(attr) => attr.set_bytes_ref(bytes),
      Attributes::BytesTerm(attr) => AttributeSource::set_bytes_ref(attr, bytes),
      Attributes::BinaryTokenStream(attr) => attr.set_bytes_ref(bytes),
      #[cfg(test)]
      Attributes::RandomTokenStream(attr) => attr.set_bytes_ref(bytes),
    }
  }

  fn get_position_increment(&self) -> Result<i32> {
    match self {
      Attributes::PackedToken(attr) => attr.get_position_increment(),
      Attributes::BytesTerm(attr) => attr.get_position_increment(),
      Attributes::BinaryTokenStream(attr) => attr.get_position_increment(),
      #[cfg(test)]
      Attributes::RandomTokenStream(attr) => attr.get_position_increment(),
    }
  }

  fn set_position_increment(&mut self, position_increment: i32) -> Result<()> {
    match self {
      Attributes::PackedToken(attr) => attr.set_position_increment(position_increment),
      Attributes::BytesTerm(attr) => attr.set_position_increment(position_increment),
      Attributes::BinaryTokenStream(attr) => attr.set_position_increment(position_increment),
      #[cfg(test)]
      Attributes::RandomTokenStream(attr) => attr.set_position_increment(position_increment),
    }
  }

  fn get_payload(&self) -> Result<Option<&BytesRef<Vec<u8>>>> {
    match self {
      Attributes::PackedToken(attr) => attr.get_payload(),
      Attributes::BytesTerm(attr) => attr.get_payload(),
      Attributes::BinaryTokenStream(attr) => attr.get_payload(),
      #[cfg(test)]
      Attributes::RandomTokenStream(attr) => attr.get_payload(),
    }
  }

  fn set_payload(&mut self, payload: Option<BytesRef<Vec<u8>>>) -> Result<()> {
    match self {
      Attributes::PackedToken(attr) => attr.set_payload(payload),
      Attributes::BytesTerm(attr) => attr.set_payload(payload),
      Attributes::BinaryTokenStream(attr) => attr.set_payload(payload),
      #[cfg(test)]
      Attributes::RandomTokenStream(attr) => attr.set_payload(payload),
    }
  }

  fn get_bytes_ref(&mut self) -> Result<Option<&BytesRef<Vec<u8>>>> {
    match self {
      Attributes::PackedToken(attr) => AttributeSource::get_bytes_ref(attr),
      Attributes::BytesTerm(attr) => AttributeSource::get_bytes_ref(attr),
      Attributes::BinaryTokenStream(attr) => AttributeSource::get_bytes_ref(attr),
      #[cfg(test)]
      Attributes::RandomTokenStream(attr) => AttributeSource::get_bytes_ref(attr),
    }
  }

  fn set_term_frequency(&mut self, term_frequency: i32) -> Result<()> {
    match self {
      Attributes::PackedToken(attr) => attr.set_term_frequency(term_frequency),
      Attributes::BytesTerm(attr) => attr.set_term_frequency(term_frequency),
      Attributes::BinaryTokenStream(attr) => attr.set_term_frequency(term_frequency),
      #[cfg(test)]
      Attributes::RandomTokenStream(attr) => attr.set_term_frequency(term_frequency),
    }
  }

  fn get_term_frequency(&self) -> Result<i32> {
    match self {
      Attributes::PackedToken(attr) => attr.get_term_frequency(),
      Attributes::BytesTerm(attr) => attr.get_term_frequency(),
      Attributes::BinaryTokenStream(attr) => attr.get_term_frequency(),
      #[cfg(test)]
      Attributes::RandomTokenStream(attr) => attr.get_term_frequency(),
    }
  }

  fn set_position_length(&mut self, position_length: i32) -> Result<()> {
    match self {
      Attributes::PackedToken(attr) => attr.set_position_length(position_length),
      Attributes::BytesTerm(attr) => attr.set_position_length(position_length),
      Attributes::BinaryTokenStream(attr) => attr.set_position_length(position_length),
      #[cfg(test)]
      Attributes::RandomTokenStream(attr) => attr.set_position_length(position_length),
    }
  }

  fn get_position_length(&self) -> Result<i32> {
    match self {
      Attributes::PackedToken(attr) => attr.get_position_length(),
      Attributes::BytesTerm(attr) => attr.get_position_length(),
      Attributes::BinaryTokenStream(attr) => attr.get_position_length(),
      #[cfg(test)]
      Attributes::RandomTokenStream(attr) => attr.get_position_length(),
    }
  }

  fn get_flags(&self) -> Result<i32> {
    match self {
      Attributes::PackedToken(attr) => attr.get_flags(),
      Attributes::BytesTerm(attr) => attr.get_flags(),
      Attributes::BinaryTokenStream(attr) => attr.get_flags(),
      #[cfg(test)]
      Attributes::RandomTokenStream(attr) => attr.get_flags(),
    }
  }

  fn set_flags(&mut self, flags: i32) -> Result<()> {
    match self {
      Attributes::PackedToken(attr) => attr.set_flags(flags),
      Attributes::BytesTerm(attr) => attr.set_flags(flags),
      Attributes::BinaryTokenStream(attr) => attr.set_flags(flags),
      #[cfg(test)]
      Attributes::RandomTokenStream(attr) => attr.set_flags(flags),
    }
  }

  fn type_(&self) -> Result<&str> {
    match self {
      Attributes::PackedToken(attr) => attr.type_(),
      Attributes::BytesTerm(attr) => attr.type_(),
      Attributes::BinaryTokenStream(attr) => attr.type_(),
      #[cfg(test)]
      Attributes::RandomTokenStream(attr) => attr.type_(),
    }
  }

  fn set_type(&mut self, type_: &str) -> Result<()> {
    match self {
      Attributes::PackedToken(attr) => attr.set_type(type_),
      Attributes::BytesTerm(attr) => attr.set_type(type_),
      Attributes::BinaryTokenStream(attr) => attr.set_type(type_),
      #[cfg(test)]
      Attributes::RandomTokenStream(attr) => attr.set_type(type_),
    }
  }

  fn end_attributes(&mut self) {
    match self {
      Attributes::PackedToken(attr) => attr.end_attributes(),
      Attributes::BytesTerm(attr) => attr.end_attributes(),
      Attributes::BinaryTokenStream(attr) => attr.end_attributes(),
      #[cfg(test)]
      Attributes::RandomTokenStream(attr) => attr.end_attributes(),
    }
  }

  fn clear_attributes(&mut self) -> Result<()> {
    match self {
      Attributes::PackedToken(attr) => attr.clear_attributes(),
      Attributes::BytesTerm(attr) => attr.clear_attributes(),
      Attributes::BinaryTokenStream(attr) => attr.clear_attributes(),
      #[cfg(test)]
      Attributes::RandomTokenStream(attr) => attr.clear_attributes(),
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
  const TERM_FREQUENCY_IS_ABSENT: bool = true;

  fn end_attributes(&mut self) {}

  fn clear_attributes(&mut self) -> Result<()> {
    Ok(())
  }
}

macro_rules! define_attribute_source_enum {
    (
        $enum_name:ident,
        [$($V:ident),+ $(,)?]
    ) => {
        pub enum $enum_name<$($V),+> {
            $($V($V)),+
        }

        impl<$($V),+> AttributeSource for $enum_name<$($V),+>
        where
            $($V: AttributeSource,)+
        {
            fn length(&self) -> Result<usize> {
                match self {
                    $(Self::$V(t) => t.length(),)+
                }
            }

            fn copy_buffer(&mut self, buffer: &[char], offset: usize, length: usize) -> Result<()> {
                match self {
                    $(Self::$V(t) => t.copy_buffer(buffer, offset, length),)+
                }
            }

            fn buffer_mut(&mut self) -> Result<&mut [char]> {
                match self {
                    $(Self::$V(t) => t.buffer_mut(),)+
                }
            }

            fn buffer(&self) -> Result<&[char]> {
                match self {
                    $(Self::$V(t) => t.buffer(),)+
                }
            }

            fn resize_buffer(&mut self, new_size: usize) -> Result<&mut [char]> {
                match self {
                    $(Self::$V(t) => t.resize_buffer(new_size),)+
                }
            }

            fn set_length(&mut self, length: usize) -> Result<&mut Self> {
                match self {
                    $(Self::$V(t) => {
                        t.set_length(length)?;
                        Ok(self)
                    },)+
                }
            }

            fn set_empty(&mut self) -> Result<&mut Self> {
                match self {
                    $(Self::$V(t) => {
                        t.set_empty()?;
                        Ok(self)
                    },)+
                }
            }

            fn append_range(&mut self, csq: Option<&str>, start: usize, end: usize) -> Result<&mut Self> {
                match self {
                    $(Self::$V(t) => {
                        t.append_range(csq, start, end)?;
                        Ok(self)
                    },)+
                }
            }

            fn append_char(&mut self, c: char) -> Result<&mut Self> {
                match self {
                    $(Self::$V(t) => {
                        t.append_char(c)?;
                        Ok(self)
                    },)+
                }
            }

            fn append_str(&mut self, s: Option<&str>) -> Result<&mut Self> {
                match self {
                    $(Self::$V(t) => {
                        t.append_str(s)?;
                        Ok(self)
                    },)+
                }
            }

            fn append_term_attribute<TermAtt>(&mut self, term_att: Option<&mut TermAtt>) -> Result<&mut Self>
            where
                TermAtt: CharTermAttribute,
            {
                match self {
                    $(Self::$V(t) => {
                        t.append_term_attribute(term_att)?;
                        Ok(self)
                    },)+
                }
            }

            fn start_offset(&self) -> Result<i32> {
                match self {
                    $(Self::$V(t) => t.start_offset(),)+
                }
            }

            fn set_offset(&mut self, start_offset: i32, end_offset: i32) -> Result<()> {
                match self {
                    $(Self::$V(t) => t.set_offset(start_offset, end_offset),)+
                }
            }

            fn end_offset(&self) -> Result<i32> {
                match self {
                    $(Self::$V(t) => t.end_offset(),)+
                }
            }

            fn set_bytes_ref(&mut self, bytes: Option<BytesRef<Vec<u8>>>) -> Result<()> {
                match self {
                    $(Self::$V(t) => t.set_bytes_ref(bytes),)+
                }
            }

            fn get_position_increment(&self) -> Result<i32> {
                match self {
                    $(Self::$V(t) => t.get_position_increment(),)+
                }
            }

            fn set_position_increment(&mut self, position_increment: i32) -> Result<()> {
                match self {
                    $(Self::$V(t) => t.set_position_increment(position_increment),)+
                }
            }

            fn get_payload(&self) -> Result<Option<&BytesRef<Vec<u8>>>> {
                match self {
                    $(Self::$V(t) => t.get_payload(),)+
                }
            }
            fn set_payload(&mut self, payload: Option<BytesRef<Vec<u8>>>) -> Result<()> {
                match self {
                    $(Self::$V(t) => t.set_payload(payload),)+
                }
            }

            fn get_bytes_ref(&mut self) -> Result<Option<&BytesRef<Vec<u8>>>> {
                match self {
                    $(Self::$V(t) => t.get_bytes_ref(),)+
                }
            }

            fn get_term_frequency(&self) -> Result<i32> {
                match self {
                    $(Self::$V(t) => t.get_term_frequency(),)+
                }
            }
            fn set_term_frequency(&mut self, term_frequency: i32) -> Result<()> {
                match self {
                    $(Self::$V(t) => t.set_term_frequency(term_frequency),)+
                }
            }
            fn set_position_length(&mut self,position_length: i32) -> Result<()> {
                match self {
                    $(Self::$V(t) => t.set_position_length(position_length),)+
                }
            }
            fn get_position_length(&self) -> Result<i32> {
                match self {
                    $(Self::$V(t) => t.get_position_length(),)+
                }
            }
            fn is_keyword(&self) -> Result<bool>{
                match self {
                    $(Self::$V(t) => t.is_keyword(),)+
                }
            }
            fn set_keyword(&mut self,is_keyword: bool) -> Result<()>{
                match self {
                    $(Self::$V(t) => t.set_keyword(is_keyword),)+
                }
            }
            fn get_flags(&self) -> Result<i32>{
                match self {
                    $(Self::$V(t) => t.get_flags(),)+
                }
            }
            fn set_flags(&mut self,flags: i32) -> Result<()>{
                match self {
                    $(Self::$V(t) => t.set_flags(flags),)+
                }
            }

            fn set_boost(&mut self,boost: f32) -> Result<()>{
                match self {
                    $(Self::$V(t) => t.set_boost(boost),)+
                }
            }
            fn get_boost(&self) -> Result<f32>{
                match self {
                    $(Self::$V(t) => t.get_boost(),)+
                }
            }
            fn set_max_non_competitive_boost(&mut self, max_non_competitive_boost: f32) -> Result<()> {
                match self {
                    $(Self::$V(t) => t.set_max_non_competitive_boost(max_non_competitive_boost),)+
                }
            }
            fn get_max_non_competitive_boost(&self) -> Result<f32> {
                match self {
                    $(Self::$V(t) => t.get_max_non_competitive_boost(),)+
                }
            }
            fn set_competitive_term(&mut self, competitive_term: Option<BytesRef<Vec<u8>>>) -> Result<()> {
                match self {
                    $(Self::$V(t) => t.set_competitive_term(competitive_term),)+
                }
            }
            fn get_competitive_term(&self) -> Result<Option<&BytesRef<Vec<u8>>>> {
                match self {
                    $(Self::$V(t) => t.get_competitive_term(),)+
                }
            }
            fn type_(&self) -> Result<&str> {
                match self {
                    $(Self::$V(t) => t.type_(),)+
                }
            }
            fn set_type(&mut self, type_: &str) -> Result<()> {
                match self {
                    $(Self::$V(t) => t.set_type(type_),)+
                }
            }
            fn end_attributes(&mut self) {
                match self {
                    $(Self::$V(t) => t.end_attributes(),)+
                }
            }

            fn clear_attributes(&mut self) -> Result<()> {
                match self {
                    $(Self::$V(t) => t.clear_attributes(),)+
                }
            }
        }
    };
}
define_attribute_source_enum!(AttributeSourceEnum2, [A, B]);
