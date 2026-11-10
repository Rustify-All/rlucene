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
use std::borrow::Cow;
use std::sync::Arc;

#[cfg(test)]
use crate::core::analysis::char_filter::CharFilter;
#[cfg(test)]
use crate::test_framework::core::analysis::char_filter::{CharFilter1, CharFilter2};
#[cfg(test)]
use crate::test_framework::core::analysis::mock_char_filter::MockCharFilter;
#[cfg(test)]
use crate::test_framework::core::analysis::standard::test_standard_analyzer::SpoonFeedMaxCharsReaderWrapper;

use crate::core::analysis::reusable_string_reader::ReusableStringReader;
use crate::core::analysis::tokenizer::IllegalStateReader;
use crate::core::util::error::lucene_error::{LuceneError, Result};

pub trait Reader {
  /// Reads a single character. Returns -1 on EOF
  fn read(&mut self) -> Result<i32> {
    let mut cb = [char::from(0); 1];
    if self.read_range(&mut cb, 0, 1)? == -1 {
      return Ok(-1);
    }
    Ok(cb[0] as i32)
  }
  fn read_buf(&mut self, cbuf: &mut [char]) -> Result<i32> {
    self.read_range(cbuf, 0, cbuf.len())
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
  String(StringReader),
  #[cfg(test)]
  CharFilter1(CharFilter1),
  #[cfg(test)]
  CharFilter2(CharFilter2),
  #[cfg(test)]
  MockCharFilter(MockCharFilter),
  #[cfg(test)]
  SpoonFeedMaxCharsReaderWrapper(SpoonFeedMaxCharsReaderWrapper),
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
      #[cfg(test)]
      ReaderEnum::MockCharFilter(r) => r.correct_offset(corrected),
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
      ReaderEnum::String(r) => r.read(),
      #[cfg(test)]
      ReaderEnum::CharFilter1(r) => r.read(),
      #[cfg(test)]
      ReaderEnum::CharFilter2(r) => r.read(),
      #[cfg(test)]
      ReaderEnum::MockCharFilter(r) => r.read(),
      #[cfg(test)]
      ReaderEnum::SpoonFeedMaxCharsReaderWrapper(r) => r.read(),
    }
  }

  fn read_buf(&mut self, cbuf: &mut [char]) -> Result<i32> {
    match self {
      ReaderEnum::ReusedString(r) => r.read_buf(cbuf),
      ReaderEnum::IllegalState(r) => r.read_buf(cbuf),
      ReaderEnum::String(r) => r.read_buf(cbuf),
      #[cfg(test)]
      ReaderEnum::CharFilter1(r) => r.read_buf(cbuf),
      #[cfg(test)]
      ReaderEnum::CharFilter2(r) => r.read_buf(cbuf),
      #[cfg(test)]
      ReaderEnum::MockCharFilter(r) => r.read_buf(cbuf),
      #[cfg(test)]
      ReaderEnum::SpoonFeedMaxCharsReaderWrapper(r) => r.read_buf(cbuf),
    }
  }

  fn read_range(&mut self, buf: &mut [char], off: usize, len: usize) -> Result<i32> {
    match self {
      ReaderEnum::ReusedString(r) => r.read_range(buf, off, len),
      ReaderEnum::IllegalState(r) => r.read_range(buf, off, len),
      ReaderEnum::String(r) => r.read_range(buf, off, len),
      #[cfg(test)]
      ReaderEnum::CharFilter1(r) => r.read_range(buf, off, len),
      #[cfg(test)]
      ReaderEnum::CharFilter2(r) => r.read_range(buf, off, len),
      #[cfg(test)]
      ReaderEnum::MockCharFilter(r) => r.read_range(buf, off, len),
      #[cfg(test)]
      ReaderEnum::SpoonFeedMaxCharsReaderWrapper(r) => r.read_range(buf, off, len),
    }
  }

  fn close(&mut self) -> Result<()> {
    match self {
      ReaderEnum::ReusedString(r) => r.close(),
      ReaderEnum::IllegalState(r) => r.close(),
      ReaderEnum::String(r) => r.close(),
      #[cfg(test)]
      ReaderEnum::CharFilter1(r) => CharFilter::close(r),
      #[cfg(test)]
      ReaderEnum::CharFilter2(r) => CharFilter::close(r),
      #[cfg(test)]
      ReaderEnum::MockCharFilter(r) => CharFilter::close(r),
      #[cfg(test)]
      ReaderEnum::SpoonFeedMaxCharsReaderWrapper(r) => r.close(),
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
    ReaderEnum::String(StringReader::from(text))
  }
}

impl From<Arc<String>> for ReaderEnum {
  fn from(text: Arc<String>) -> Self {
    ReaderEnum::String(StringReader::from(text))
  }
}
impl From<ReusableStringReader> for ReaderEnum {
  fn from(reader: ReusableStringReader) -> Self {
    ReaderEnum::ReusedString(reader)
  }
}
impl From<IllegalStateReader> for ReaderEnum {
  fn from(reader: IllegalStateReader) -> Self {
    ReaderEnum::IllegalState(reader)
  }
}
impl From<StringReader> for ReaderEnum {
  fn from(reader: StringReader) -> Self {
    ReaderEnum::String(reader)
  }
}

/// A character stream whose source is a string.
///
/// Input is stored as UTF-8; reads produce Unicode scalar values (`char`).
#[derive(Debug, Clone)]
pub struct StringReader {
  source: Option<Arc<String>>,
  next: usize,
}

impl StringReader {
  pub fn new<'a, T>(s: T) -> Self
  where
    T: Into<Cow<'a, str>>,
  {
    Self::from(s.into().into_owned())
  }

  fn check_range(buf_len: usize, off: usize, len: usize) -> Result<usize> {
    let end = off
      .checked_add(len)
      .ok_or_else(|| LuceneError::illegal_argument("IndexOutOfBounds: off+len overflow"))?;
    if off > buf_len || end > buf_len {
      return Err(LuceneError::illegal_argument(
        "IndexOutOfBounds: off+len exceeds buffer length",
      ));
    }
    Ok(end)
  }

  #[inline]
  pub(crate) fn read_range_with_units<const COUNT_UTF16: bool>(
    &mut self,
    buf: &mut [char],
    off: usize,
    len: usize,
  ) -> Result<(i32, usize)> {
    let s = self
      .source
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("Stream closed"))?;
    Self::check_range(buf.len(), off, len)?;
    if self.next >= s.len() {
      return Ok((if len == 0 { 0 } else { -1 }, 0));
    }
    let bytes = &s.as_bytes()[self.next..];
    let to_read = len.min(bytes.len());
    if to_read >= 16 && (bytes[0] | bytes[to_read - 1]) < 0x80 && bytes[..to_read].is_ascii() {
      for (dest, &byte) in buf[off..off + to_read].iter_mut().zip(bytes) {
        *dest = byte as char;
      }
      self.next += to_read;
      return Ok((to_read as i32, to_read));
    }
    let (read, units, consumed) =
      Self::read_utf8::<COUNT_UTF16>(&s[self.next..], &mut buf[off..off + len]);
    self.next += consumed;
    Ok((read, units))
  }

  #[inline(never)]
  fn read_utf8<const COUNT_UTF16: bool>(source: &str, buf: &mut [char]) -> (i32, usize, usize) {
    let bytes = source.as_bytes();
    let to_read = buf.len().min(bytes.len());
    let ascii_prefix = if to_read >= 64 && (bytes[0] | bytes[15]) < 0x80 {
      bytes[..to_read]
        .as_chunks::<16>()
        .0
        .iter()
        .take_while(|&&block| wide::u8x16::new(block).to_bitmask() == 0)
        .count()
        * 16
    } else {
      0
    };
    for (dest, &byte) in buf[..ascii_prefix].iter_mut().zip(bytes) {
      *dest = byte as char;
    }
    if ascii_prefix != 0 && buf.len() - ascii_prefix >= 16 {
      let (read, units, consumed) =
        Self::read_ascii_runs::<COUNT_UTF16>(&source[ascii_prefix..], &mut buf[ascii_prefix..]);
      return (
        (ascii_prefix + read) as i32,
        ascii_prefix + units,
        ascii_prefix + consumed,
      );
    }
    let mut read = ascii_prefix;
    let mut supplementary = 0;
    // The prefix consists of ASCII bytes, so the suffix starts on a character boundary.
    let mut chars = source[ascii_prefix..].chars();
    for (dest, ch) in buf[ascii_prefix..].iter_mut().zip(chars.by_ref()) {
      *dest = ch;
      read += 1;
      if COUNT_UTF16 {
        supplementary += usize::from(ch.len_utf16() == 2);
      }
    }
    let consumed = source.len() - chars.as_str().len();
    (read as i32, read + supplementary, consumed)
  }
  #[inline(never)]
  fn read_ascii_runs<const COUNT_UTF16: bool>(
    source: &str,
    buf: &mut [char],
  ) -> (usize, usize, usize) {
    let mut chars = source.chars();
    let mut read = 0;
    let mut supplementary = 0;
    let (groups, tail) = buf.as_chunks_mut::<16>();
    for group in groups {
      let rest = chars.as_str();
      if rest.is_empty() {
        break;
      }
      if let Some(&bytes) = rest.as_bytes().first_chunk::<16>()
        && wide::u8x16::new(bytes).to_bitmask() == 0
      {
        *group = bytes.map(char::from);
        chars = rest[16..].chars();
        read += 16;
      } else {
        for (dest, ch) in group.iter_mut().zip(chars.by_ref()) {
          *dest = ch;
          read += 1;
          if COUNT_UTF16 {
            supplementary += usize::from(ch.len_utf16() == 2);
          }
        }
      }
    }
    for (dest, ch) in tail.iter_mut().zip(chars.by_ref()) {
      *dest = ch;
      read += 1;
      if COUNT_UTF16 {
        supplementary += usize::from(ch.len_utf16() == 2);
      }
    }
    (
      read,
      read + supplementary,
      source.len() - chars.as_str().len(),
    )
  }
}

impl From<Arc<String>> for StringReader {
  fn from(text: Arc<String>) -> Self {
    Self {
      source: Some(text),
      next: 0,
    }
  }
}

impl From<String> for StringReader {
  fn from(text: String) -> Self {
    Self::from(Arc::new(text))
  }
}

impl Reader for StringReader {
  fn read(&mut self) -> Result<i32> {
    let s = self
      .source
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("Stream closed"))?;
    if self.next >= s.len() {
      return Ok(-1);
    }
    let ch = expect_invariant!(
      s[self.next..].chars().next(),
      "reader position stays on a character boundary"
    );
    self.next += ch.len_utf8();
    Ok(ch as i32)
  }

  fn read_range(&mut self, buf: &mut [char], off: usize, len: usize) -> Result<i32> {
    Ok(self.read_range_with_units::<false>(buf, off, len)?.0)
  }

  fn close(&mut self) -> Result<()> {
    self.source = None;
    Ok(())
  }
}
