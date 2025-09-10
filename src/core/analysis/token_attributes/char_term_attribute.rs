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
use crate::core::util::attribute::Attribute;
use crate::core::util::error::lucene_error::Result;

/// The term text of a `Token`.
pub trait CharTermAttribute: Attribute {
    fn length(&self) -> usize;
    /// Copies the contents of `buffer[offset..offset+length]` into the internal term buffer.
    ///
    /// # Parameters
    ///
    /// - `buffer`: the source character slice  
    /// - `offset`: index of first character to copy  
    /// - `length`: number of characters to copy  
    fn copy_buffer(&mut self, buffer: &[char], offset: usize, length: usize);

    /// Returns the internal term buffer which you can directly alter.
    ///
    /// If the buffer is too small for your token, use [`resize_buffer`](Self::resize_buffer) to grow it.
    /// After altering the buffer be sure to call [`set_length`](Self::set_length) to record the number
    /// of valid characters placed into it.
    ///
    /// **Note:** the returned slice may be larger than the valid length.
    fn buffer_mut(&mut self) -> &mut [char];
    fn buffer(&self) -> &[char];

    /// Grows the term buffer to at least `new_size`, preserving existing content.
    ///
    /// # Returns
    ///
    /// A mutable slice to the new buffer (with `len() >= new_size`).
    fn resize_buffer(&mut self, new_size: usize) -> &mut [char];

    /// Sets the number of valid characters (length of the term) in the term buffer.
    ///
    /// Use this to truncate the buffer or to synchronize with external buffer manipulation.
    /// To grow the buffer, call [`resize_buffer`](Self::resize_buffer) first.
    ///
    /// # Returns
    ///
    /// `self` for chaining.
    fn set_length(&mut self, length: usize) -> Result<&mut Self>;

    /// Resets the term buffer to zero length.
    ///
    /// Use before appending via the `Appendable` interface.
    ///
    /// # Returns
    ///
    /// `self` for chaining.
    fn set_empty(&mut self) -> &mut Self;

    /// Appends the subsequence `csq[start..end]` to this term.
    ///
    /// # Returns
    ///
    /// `self` for chaining.
    fn append_range(&mut self, csq: &str, start: usize, end: usize) -> &mut Self;

    /// Appends a single character `c` to this term.
    ///
    /// # Returns
    ///
    /// `self` for chaining.
    fn append_char(&mut self, c: char) -> &mut Self;

    /// Appends the specified `&str` to this term.
    ///
    /// # Returns
    ///
    /// `self` for chaining.
    fn append_str(&mut self, s: Option<&str>) -> &mut Self;

    /// Appends the contents of another `CharTermAttribute` to this term.
    ///
    /// # Returns
    ///
    /// `self` for chaining.
    fn append_term_attribute<C>(&mut self, term_att: Option<&mut C>) -> &mut Self
    where
        C: CharTermAttribute;
}
