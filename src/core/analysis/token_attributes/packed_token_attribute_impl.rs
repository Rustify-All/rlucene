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
use crate::core::analysis::token_attributes::char_term_attribute_impl::{
  CharTermAttributeImpl, CharTermAttributeImplBase,
};
#[cfg(test)]
use crate::core::analysis::token_attributes::flags_attribute::FlagsAttribute;
use crate::core::analysis::token_attributes::offset_attribute::OffsetAttribute;
#[cfg(test)]
use crate::core::analysis::token_attributes::payload_attribute::PayloadAttribute;
use crate::core::analysis::token_attributes::position_increment_attribute::PositionIncrementAttribute;
use crate::core::analysis::token_attributes::position_length_attribute::PositionLengthAttribute;
use crate::core::analysis::token_attributes::term_frequency_attribute::TermFrequencyAttribute;
use crate::core::analysis::token_attributes::term_to_bytes_ref_attribute::TermToBytesRefAttribute;
use crate::core::analysis::token_attributes::type_attribute::{DEFAULT_TYPE, TypeAttribute};
use crate::core::index::BytesRef;
use crate::core::util::attribute::Attribute;
use crate::core::util::attribute_impl::AttributeImpl;
use crate::core::util::attribute_source::AttributeSource;
use crate::core::util::error::lucene_error::{LuceneError, Result};
#[cfg(test)]
use crate::test_framework::core::analysis::base_token_stream_test_case::{
  CheckClearAttributesAttribute, CheckClearAttributesAttributeImpl,
};
#[cfg(test)]
use crate::test_framework::core::analysis::token::TokenBase;
use std::borrow::Cow;
#[cfg(any(test, debug_assertions))]
use std::collections::HashSet;
use std::fmt::{Display, Formatter};
use std::hash::{Hash, Hasher};
#[cfg(any(test, debug_assertions))]
use std::sync::Arc;

pub type PackedTokenAttribute = CharTermAttributeImpl<PackedTokenAttributeImpl>;
/// Default implementation of the common attributes used by Lucene:
///
/// - [`CharTermAttribute`]
/// - [`TypeAttribute`]
/// - [`PositionIncrementAttribute`]
/// - [`PositionLengthAttribute`]
/// - [`OffsetAttribute`]
/// - [`TermFrequencyAttribute`]
pub struct PackedTokenAttributeImpl {
  start_offset: i32,
  end_offset: i32,
  type_: Cow<'static, str>,
  position_increment: i32,
  position_length: i32,
  term_frequency: i32,
  #[cfg(test)]
  check_clear_attributes: CheckClearAttributesAttributeImpl,
  #[cfg(any(test, debug_assertions))]
  attribute: Arc<HashSet<String>>,
  #[cfg(test)]
  pub token: TokenBase,
}

impl PackedTokenAttributeImpl {
  pub fn new() -> Result<CharTermAttributeImpl<Self>> {
    // TODO is there a better way to do this?
    #[cfg(any(test, debug_assertions))]
    let mut attribute = HashSet::new();
    #[cfg(any(test, debug_assertions))]
    {
      attribute.insert(<Self as TypeAttribute>::ATTRIBUTE_NAME.to_string());
      attribute.insert(<Self as PositionIncrementAttribute>::ATTRIBUTE_NAME.to_string());
      attribute.insert(<Self as PositionLengthAttribute>::ATTRIBUTE_NAME.to_string());
      attribute.insert(<Self as OffsetAttribute>::ATTRIBUTE_NAME.to_string());
      attribute.insert(<Self as TermFrequencyAttribute>::ATTRIBUTE_NAME.to_string());
    }
    #[cfg(test)]
    let token = TokenBase::default();
    #[cfg(test)]
    {
      attribute.insert(<Self as CheckClearAttributesAttribute>::ATTRIBUTE_NAME.to_string());
      attribute.extend(token.get_attribute_name()?.iter().cloned())
    }
    let sub = Self {
      start_offset: 0,
      end_offset: 0,
      type_: Cow::Borrowed(DEFAULT_TYPE),
      position_increment: 1,
      position_length: 1,
      term_frequency: 1,
      #[cfg(test)]
      check_clear_attributes: CheckClearAttributesAttributeImpl::new(),
      #[cfg(any(test, debug_assertions))]
      attribute: Arc::new(attribute),
      #[cfg(test)]
      token,
    };
    CharTermAttributeImpl::with_sub(sub)
  }
}

impl Attribute for PackedTokenAttributeImpl {
  #[cfg(any(test, debug_assertions))]
  fn get_attribute_name(&self) -> Result<&HashSet<String>> {
    Ok(&self.attribute)
  }
}

impl TypeAttribute for PackedTokenAttributeImpl {
  /// Returns this Token's lexical type. Defaults to "word".
  fn type_(&self) -> &str {
    self.type_.as_ref()
  }
  /// Set the lexical type.
  #[inline]
  fn set_type(&mut self, type_: &str) {
    match &mut self.type_ {
      Cow::Borrowed(value) if type_ == DEFAULT_TYPE => *value = DEFAULT_TYPE,
      Cow::Borrowed(_) => self.type_ = Cow::Owned(type_.to_owned()),
      Cow::Owned(value) => type_.clone_into(value),
    }
  }
}
impl PositionIncrementAttribute for PackedTokenAttributeImpl {
  /// Set the position increment. The default value is one.
  fn set_position_increment(&mut self, position_increment: i32) -> Result<()> {
    if position_increment < 0 {
      return Err(LuceneError::illegal_state(format!(
        "Increment must be zero or greater: {position_increment}"
      )));
    }
    self.position_increment = position_increment;
    Ok(())
  }

  /// Returns the position increment of this Token.
  fn get_position_increment(&self) -> i32 {
    self.position_increment
  }
}
impl PositionLengthAttribute for PackedTokenAttributeImpl {
  /// Set the position length of this Token.
  fn set_position_length(&mut self, position_length: i32) -> Result<()> {
    if position_length < 1 {
      return Err(LuceneError::illegal_argument(format!(
        "Position length must be 1 or greater: got {position_length}"
      )));
    }
    self.position_length = position_length;
    Ok(())
  }

  /// Returns the position length of this Token.
  fn get_position_length(&self) -> i32 {
    self.position_length
  }
}
impl OffsetAttribute for PackedTokenAttributeImpl {
  /// Returns this token’s starting offset—the position of the first character corresponding to this token in the source text.
  ///
  /// Note that the difference between [`end_offset()`](Self::end_offset) and `start_offset()` may not equal `term_text.len()`, as the term text may have been altered by a stemmer or another filter.
  fn start_offset(&self) -> i32 {
    self.start_offset
  }
  /// Set the starting and ending offset.
  fn set_offset(&mut self, start_offset: i32, end_offset: i32) -> Result<()> {
    if start_offset < 0 || end_offset < start_offset {
      return Err(LuceneError::illegal_argument(format!(
        "start_offset must be non-negative, and end_offset must be >= start_offset; got start_offset={start_offset}, end_offset={end_offset}"
      )));
    }
    self.start_offset = start_offset;
    self.end_offset = end_offset;
    Ok(())
  }
  /// Returns this token’s ending offset—one greater than the position of the last character corresponding to this token in the source text.
  /// The length of the token in the source text is `(end_offset() - start_offset())`.
  fn end_offset(&self) -> i32 {
    self.end_offset
  }
}
impl TermFrequencyAttribute for PackedTokenAttributeImpl {
  fn set_term_frequency(&mut self, term_frequency: i32) -> Result<()> {
    if term_frequency < 1 {
      return Err(LuceneError::illegal_argument(format!(
        "Term frequency must be 1 or greater; got {term_frequency}"
      )));
    }
    self.term_frequency = term_frequency;
    Ok(())
  }

  fn get_term_frequency(&self) -> i32 {
    self.term_frequency
  }
}
impl Clone for PackedTokenAttributeImpl {
  fn clone(&self) -> Self {
    Self {
      start_offset: self.start_offset,
      end_offset: self.end_offset,
      type_: self.type_.clone(),
      position_increment: self.position_increment,
      position_length: self.position_length,
      term_frequency: self.term_frequency,
      #[cfg(test)]
      check_clear_attributes: self.check_clear_attributes.clone(),
      #[cfg(any(test, debug_assertions))]
      attribute: self.attribute.clone(),
      #[cfg(test)]
      token: self.token.clone(),
    }
  }
}

impl AttributeImpl for PackedTokenAttributeImpl {
  /// Resets the attributes
  #[inline]
  fn clear(&mut self) {
    self.position_increment = 1;
    self.position_length = 1;
    self.term_frequency = 1;
    self.start_offset = 0;
    self.end_offset = 0;
    TypeAttribute::set_type(self, DEFAULT_TYPE);
    #[cfg(test)]
    self.check_clear_attributes.clear();
    #[cfg(test)]
    self.token.clear()
  }

  /// Resets the attributes at end
  fn end(&mut self) {
    self.position_increment = 0;
  }

  type AttributeImpl = PackedTokenAttributeImpl;

  fn copy_to(&self, to: &mut Self::AttributeImpl) -> Result<()> {
    to.position_increment = self.position_increment;
    to.position_length = self.position_length;
    to.start_offset = self.start_offset;
    to.end_offset = self.end_offset;
    TypeAttribute::set_type(to, self.type_.as_ref());
    to.term_frequency = self.term_frequency;
    #[cfg(test)]
    self.token.copy_to(&mut to.token)?;
    Ok(())
  }
}
impl Hash for PackedTokenAttributeImpl {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    self.start_offset.hash(state);
    self.end_offset.hash(state);
    self.type_.hash(state);
    self.position_increment.hash(state);
    self.position_length.hash(state);
    self.term_frequency.hash(state);
    #[cfg(test)]
    self.token.hash(state);
  }
}
impl PartialEq for PackedTokenAttributeImpl {
  fn eq(&self, other: &Self) -> bool {
    self.start_offset == other.start_offset
      && self.end_offset == other.end_offset
      && self.position_increment == other.position_increment
      && self.position_length == other.position_length
      && self.term_frequency == other.term_frequency
      && self.type_ == other.type_
      && {
        #[cfg(test)]
        {
          self.token == other.token
        }

        #[cfg(not(test))]
        {
          true
        }
      }
  }
}
impl Display for PackedTokenAttributeImpl {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}

impl CharTermAttributeImplBase for PackedTokenAttributeImpl {}

impl CharTermAttributeImpl<PackedTokenAttributeImpl> {
  pub(crate) fn set_static_type(&mut self, type_: &'static str) -> Result<()> {
    match &mut self.sub.type_ {
      Cow::Borrowed(value) => *value = type_,
      Cow::Owned(value) => type_.clone_into(value),
    }
    Ok(())
  }
}

impl AttributeSource for CharTermAttributeImpl<PackedTokenAttributeImpl> {
  fn length(&self) -> Result<usize> {
    Ok(CharTermAttribute::length(self))
  }

  fn copy_buffer(&mut self, buffer: &[char], offset: usize, length: usize) -> Result<()> {
    CharTermAttribute::copy_buffer(self, buffer, offset, length)
  }

  fn buffer_mut(&mut self) -> Result<&mut [char]> {
    Ok(CharTermAttribute::buffer_mut(self))
  }

  fn buffer(&self) -> Result<&[char]> {
    Ok(CharTermAttribute::buffer(self))
  }

  fn resize_buffer(&mut self, new_size: usize) -> Result<&mut [char]> {
    CharTermAttribute::resize_buffer(self, new_size)
  }

  fn set_length(&mut self, length: usize) -> Result<&mut Self> {
    CharTermAttribute::set_length(self, length)?;
    Ok(self)
  }

  fn set_empty(&mut self) -> Result<&mut Self> {
    CharTermAttribute::set_empty(self);
    Ok(self)
  }

  fn append_range(&mut self, csq: Option<&str>, start: usize, end: usize) -> Result<&mut Self> {
    CharTermAttribute::append_range(self, csq, start, end)?;
    Ok(self)
  }

  fn append_char(&mut self, c: char) -> Result<&mut Self> {
    CharTermAttribute::append_char(self, c)?;
    Ok(self)
  }

  fn append_str(&mut self, s: Option<&str>) -> Result<&mut Self> {
    CharTermAttribute::append_str(self, s)?;
    Ok(self)
  }

  fn append_term_attribute<C>(&mut self, term_att: Option<&mut C>) -> Result<&mut Self>
  where
    C: CharTermAttribute,
  {
    CharTermAttribute::append_term_attribute(self, term_att)?;
    Ok(self)
  }

  fn start_offset(&self) -> Result<i32> {
    Ok(self.sub.start_offset())
  }

  fn set_offset(&mut self, start_offset: i32, end_offset: i32) -> Result<()> {
    self.sub.set_offset(start_offset, end_offset)
  }

  fn end_offset(&self) -> Result<i32> {
    Ok(self.sub.end_offset())
  }

  fn get_position_increment(&self) -> Result<i32> {
    Ok(self.sub.get_position_increment())
  }

  fn set_position_increment(&mut self, position_increment: i32) -> Result<()> {
    self.sub.set_position_increment(position_increment)
  }
  #[cfg(test)]
  fn get_payload(&self) -> Result<Option<&BytesRef<Vec<u8>>>> {
    Ok(self.sub.token.get_payload())
  }
  #[cfg(not(test))]
  fn get_payload(&self) -> Result<Option<&BytesRef<Vec<u8>>>> {
    Ok(None)
  }

  #[cfg(test)]
  fn set_payload(&mut self, payload: Option<BytesRef<Vec<u8>>>) -> Result<()> {
    self.sub.token.set_payload(payload);
    Ok(())
  }

  #[cfg(not(test))]
  fn set_payload(&mut self, payload: Option<BytesRef<Vec<u8>>>) -> Result<()> {
    let _ = payload;
    Err(LuceneError::unsupported_operation(""))
  }

  fn get_bytes_ref(&mut self) -> Result<Option<&BytesRef<Vec<u8>>>> {
    TermToBytesRefAttribute::get_bytes_ref(self)
  }

  fn set_term_frequency(&mut self, term_frequency: i32) -> Result<()> {
    self.sub.set_term_frequency(term_frequency)
  }

  fn get_term_frequency(&self) -> Result<i32> {
    Ok(self.sub.get_term_frequency())
  }

  fn set_position_length(&mut self, position_length: i32) -> Result<()> {
    self.sub.set_position_length(position_length)
  }

  fn get_position_length(&self) -> Result<i32> {
    Ok(self.sub.get_position_length())
  }

  #[cfg(test)]
  fn get_flags(&self) -> Result<i32> {
    Ok(self.sub.token.get_flags())
  }

  #[cfg(not(test))]
  fn get_flags(&self) -> Result<i32> {
    Err(LuceneError::unsupported_operation(""))
  }

  #[cfg(test)]
  fn set_flags(&mut self, flags: i32) -> Result<()> {
    self.sub.token.set_flags(flags);
    Ok(())
  }

  #[cfg(not(test))]
  fn set_flags(&mut self, flags: i32) -> Result<()> {
    let _ = flags;
    Err(LuceneError::unsupported_operation(""))
  }

  fn type_(&self) -> Result<&str> {
    Ok(self.sub.type_())
  }

  fn set_type(&mut self, type_: &str) -> Result<()> {
    self.sub.set_type(type_);
    Ok(())
  }

  fn end_attributes(&mut self) {
    self.end()
  }

  #[inline]
  fn clear_attributes(&mut self) -> Result<()> {
    self.clear();
    Ok(())
  }
}
#[cfg(test)]
impl CheckClearAttributesAttribute for PackedTokenAttributeImpl {
  fn get_and_reset_clear_called(&mut self) -> bool {
    self.check_clear_attributes.get_and_reset_clear_called()
  }
}
#[cfg(test)]
impl CheckClearAttributesAttribute for CharTermAttributeImpl<PackedTokenAttributeImpl> {
  fn get_and_reset_clear_called(&mut self) -> bool {
    self.sub.check_clear_attributes.get_and_reset_clear_called()
  }
}
