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
use crate::core::analysis::token_attributes::char_term_attribute_impl::CharTermAttributeImpl;
use crate::core::analysis::token_attributes::offset_attribute::OffsetAttribute;
use crate::core::analysis::token_attributes::position_increment_attribute::PositionIncrementAttribute;
use crate::core::analysis::token_attributes::position_length_attribute::PositionLengthAttribute;
use crate::core::analysis::token_attributes::term_frequency_attribute::TermFrequencyAttribute;
use crate::core::analysis::token_attributes::type_attribute::{DEFAULT_TYPE, TypeAttribute};
use crate::core::util::attribute::Attribute;
use crate::core::util::attribute_impl::AttributeImpl;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::fmt::{Display, Formatter};
use std::hash::{Hash, Hasher};
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
    type_: String,
    position_increment: i32,
    position_length: i32,
    term_frequency: i32,
    pub(crate) base: CharTermAttributeImpl,
}
impl Default for PackedTokenAttributeImpl {
    fn default() -> Self {
        Self::new()
    }
}

impl PackedTokenAttributeImpl {
    pub fn new() -> Self {
        Self {
            start_offset: 0,
            end_offset: 0,
            type_: DEFAULT_TYPE.to_string(),
            position_increment: 1,
            position_length: 1,
            term_frequency: 1,
            base: CharTermAttributeImpl::new(),
        }
    }
}

impl Attribute for PackedTokenAttributeImpl {}

impl TypeAttribute for PackedTokenAttributeImpl {
    /// Returns this Token's lexical type. Defaults to "word".
    fn type_value(&self) -> &str {
        self.type_.as_str()
    }
    /// Set the lexical type.
    fn set_type(&mut self, type_: &str) {
        self.type_ = type_.to_string();
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
impl CharTermAttribute for PackedTokenAttributeImpl {
    fn length(&self) -> usize {
        self.base.length()
    }

    fn copy_buffer(&mut self, buffer: &[char], offset: usize, length: usize) {
        self.base.copy_buffer(buffer, offset, length);
    }

    fn buffer_mut(&mut self) -> &mut [char] {
        self.base.buffer_mut()
    }

    fn buffer(&self) -> &[char] {
        self.base.buffer()
    }

    fn resize_buffer(&mut self, new_size: usize) -> &mut [char] {
        self.base.resize_buffer(new_size)
    }

    fn set_length(&mut self, length: usize) -> Result<&mut Self> {
        self.base.set_length(length)?;
        Ok(self)
    }

    fn set_empty(&mut self) -> &mut Self {
        self.base.set_empty();
        self
    }

    fn append_range(&mut self, csq: &str, start: usize, end: usize) -> &mut Self {
        self.base.append_range(csq, start, end);
        self
    }

    fn append_char(&mut self, c: char) -> &mut Self {
        self.base.append_char(c);
        self
    }

    fn append_str(&mut self, s: Option<&str>) -> &mut Self {
        self.base.append_str(s);
        self
    }

    fn append_term_attribute<C>(&mut self, term_att: Option<&mut C>) -> &mut Self
    where
        C: CharTermAttribute,
    {
        self.base.append_term_attribute(term_att);
        self
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
            base: self.base.clone(),
        }
    }
}

impl AttributeImpl for PackedTokenAttributeImpl {
    /// Resets the attributes
    fn clear(&mut self) {
        self.base.clear();
        self.start_offset = 0;
        self.end_offset = 0;
        self.type_ = DEFAULT_TYPE.to_string();
        self.position_increment = 1;
        self.position_length = 1;
        self.term_frequency = 1;
    }

    /// Resets the attributes at end
    fn end(&mut self) {
        self.base.end();
        self.position_increment = 0;
    }

    type AttributeImpl = PackedTokenAttributeImpl;

    fn copy_to(&mut self, to: &mut Self::AttributeImpl) {
        let len = self.base.length();
        let buf = self.base.buffer();
        to.base.copy_buffer(buf, 0, len);
        to.position_increment = self.position_increment;
        to.position_length = self.position_length;
        to.start_offset = self.start_offset;
        to.end_offset = self.end_offset;
        to.type_ = self.type_.clone();
        to.term_frequency = self.term_frequency;
    }
}
impl Hash for PackedTokenAttributeImpl {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.start_offset.hash(state);
        self.end_offset.hash(state);
        self.type_.hash(state);
        self.position_increment.hash(state);
        self.position_length.hash(state);
        self.term_frequency.hash(state);
        self.base.hash(state);
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
    }
}
impl Display for PackedTokenAttributeImpl {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.base.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use crate::core::analysis::token_attributes::char_term_attribute::CharTermAttribute;
    use crate::core::analysis::token_attributes::char_term_attribute_impl::tests::{
        assert_clone_is_equal, assert_copy_is_equal,
    };
    use crate::core::analysis::token_attributes::offset_attribute::OffsetAttribute;
    use crate::core::analysis::token_attributes::packed_token_attribute_impl::PackedTokenAttributeImpl;
    use crate::core::util::error::lucene_error::Result;

    #[allow(dead_code)] // for quick search
    struct TestPackedTokenAttributeImpl;
    #[test]
    fn test_clone() -> Result<()> {
        let mut t = PackedTokenAttributeImpl::new();
        t.set_offset(0, 5)?;
        let content: Vec<char> = "hello".chars().collect();
        t.copy_buffer(&content, 0, 5);
        let copy = assert_clone_is_equal(&t);
        assert_eq!(t.to_string(), copy.to_string());
        Ok(())
    }
    #[test]
    fn test_copy_to() -> Result<()> {
        let t = PackedTokenAttributeImpl::new();
        let mut copy = assert_copy_is_equal(&t);
        assert_eq!(t.to_string(), "");
        assert_eq!(copy.to_string(), "");

        let mut t = PackedTokenAttributeImpl::new();
        t.set_offset(0, 5)?;
        let content: Vec<char> = "hello".chars().collect();
        t.copy_buffer(&content, 0, 5);

        copy = assert_copy_is_equal(&t);
        assert_eq!(t.to_string(), copy.to_string());

        Ok(())
    }
    #[test]
    fn test_packed_token_attribute_factory() {
        // this test is not required in Rust Lucene
    }
    #[test]
    fn test_attribute_reflection() {
        // this test is not required in Rust Lucene
    }
}
