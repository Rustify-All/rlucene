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
use std::cmp::Ordering;
use std::fmt;
use std::fmt::Display;
use std::hash::{Hash, Hasher};

use crate::core::index::{BytesRef, BytesRefBuilder};
use crate::core::util::accountable::Accountable;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::ram_usage_estimator::{size_of_string, size_of_vec};
/// A [`Term`] represents a word from text. This is the unit of search.
/// It is composed of two elements:
/// - the text of the word, as a string,
/// - and the name of the field that the text occurred in.
///
/// Note that terms may represent more than words from text fields,
/// but also things like dates, email addresses, URLs, etc.
#[derive(Clone, Debug)]
pub struct Term {
  pub field: String,
  pub bytes: BytesRef<Vec<u8>>,
}
impl Term {
  /// Constructs a [`Term`] with the given field and bytes.
  /// Accepts a `BytesRef`, owned bytes, text, or borrowed bytes. Owned buffers
  /// are moved; borrowed inputs are copied. Existing `BytesRef` offsets are preserved.
  pub fn new<T, B>(fld: T, bytes: B) -> Self
  where
    T: Into<String>,
    B: Into<BytesRef<Vec<u8>>>,
  {
    Term {
      field: fld.into(),
      bytes: bytes.into(),
    }
  }

  /// Constructs a Term with the given field and the bytes from a builder.
  pub fn from_bytes_ref_builder<T>(fld: T, bytes_builder: BytesRefBuilder<Vec<u8>>) -> Result<Self>
  where
    T: Into<String>,
  {
    Ok(Self::new(fld, bytes_builder.get_bytes_ref_copy()?))
  }

  /// Constructs a Term with the given field and text.
  /// That accepts a Term parameter.
  pub fn from_text<T, S>(fld: T, text: S) -> Self
  where
    T: Into<String>,
    S: AsRef<str>,
  {
    let v = text.as_ref();
    Self::new(fld, BytesRef::from_string(v))
  }

  /// Constructs a Term with the given field and empty text. This serves two
  /// purposes: 1) reuse of a Term with the same field. 2) pattern for a
  /// query.
  ///
  /// Fld field's name
  pub fn from_empty<T>(fld: T) -> Self
  where
    T: Into<String>,
  {
    Term::new(fld, BytesRef::default())
  }
  /// Returns the field of this term. The field indicates the part of a
  /// document which this term came from.
  pub fn field(&self) -> &str {
    &self.field
  }

  /// Returns a human-readable form of the term text. If the term is not valid
  /// UTF-8, the raw bytes will be printed instead.
  pub fn get_string(term_text: &BytesRef<Vec<u8>>) -> Result<Cow<'_, str>> {
    match term_text
      .offset
      .checked_add(term_text.length)
      .and_then(|end| term_text.bytes.get(term_text.offset..end))
      .and_then(|bytes| std::str::from_utf8(bytes).ok())
    {
      Some(text) => Ok(Cow::Borrowed(text)),
      None => Ok(Cow::Owned(term_text.to_string())),
    }
  }

  /// Returns the text of this term. In the case of words, this is simply the
  /// text of the word. In the case of dates and other types, this is an
  /// encoding of the object as a string.
  pub fn text(&self) -> Result<Cow<'_, str>> {
    Self::get_string(&self.bytes)
  }
  /// Returns the bytes of this term, these should not be modified.
  pub fn bytes(&self) -> &BytesRef<Vec<u8>> {
    &self.bytes
  }
}
/// Clones a borrowed term, including its field name and byte buffer.
impl From<&Term> for Term {
  fn from(term: &Term) -> Self {
    term.clone()
  }
}

impl Display for Term {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    if let Some(text) = self
      .bytes
      .offset
      .checked_add(self.bytes.length)
      .and_then(|end| self.bytes.bytes.get(self.bytes.offset..end))
      .and_then(|bytes| std::str::from_utf8(bytes).ok())
    {
      return write!(f, "{}:{}", self.field, text);
    }
    match self.text() {
      Err(_) => {
        write!(f, "{}:{}", self.field, self.bytes)
      },
      Ok(v) => {
        write!(f, "{}:{}", self.field, v)
      },
    }
  }
}
impl Accountable for Term {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(size_of_string(&self.field).saturating_add(size_of_vec(&self.bytes.bytes)))
  }
}
impl PartialEq for Term {
  fn eq(&self, other: &Self) -> bool {
    if std::ptr::eq(self, other) {
      return true;
    }
    if self.field != other.field {
      return false;
    }
    self.bytes == other.bytes
  }
}

impl Eq for Term {}

impl Ord for Term {
  /// Compares two terms, returning an `Ordering`:
  ///
  /// - `Ordering::Less` if this term belongs before the argument
  /// - `Ordering::Equal` if this term is equal to the argument
  /// - `Ordering::Greater` if this term belongs after the argument
  ///
  /// The ordering of terms is first by field, then by the text (bytes).
  fn cmp(&self, other: &Self) -> Ordering {
    let field_order = self.field.cmp(&other.field);
    if field_order != Ordering::Equal {
      return field_order;
    }
    self.bytes.cmp(&other.bytes)
  }
}

impl PartialOrd for Term {
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    Some(self.cmp(other))
  }
}
impl Hash for Term {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    self.field.hash(state);
    self.bytes.hash(state);
  }
}
