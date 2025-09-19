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
use std::cmp::Ordering;
use std::fmt;
use std::fmt::Display;
use std::hash::{Hash, Hasher};

use crate::core::index::{BytesRef, BytesRefBuilder};
use crate::core::util::accountable::Accountable;
use crate::core::util::error::lucene_error::Result;
/// A `Term` represents a word from text. This is the unit of search.
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
    /// Constructs a `Term` with the given field and bytes.
    /// The provided `BytesRef` is copied when it is non-`None`.
    pub fn new<T>(fld: T, bytes: BytesRef<Vec<u8>>) -> Self
    where
        T: Into<String>,
    {
        Term {
            field: fld.into(),
            bytes,
        }
    }

    /// Constructs a Term with the given field and the bytes from a builder.
    pub fn from_bytes_ref_builder<T>(fld: T, bytes_builder: BytesRefBuilder<Vec<u8>>) -> Self
    where
        T: Into<String>,
    {
        Self::new(fld, bytes_builder.get_bytes_ref_copy())
    }

    /// Constructs a Term with the given field and text.
    /// That accepts a Term parameter.
    pub fn from_text<T>(fld: T, text: &str) -> Self
    where
        T: Into<String>,
    {
        Self::new(fld, BytesRef::from_string(text))
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
    pub fn get_string(term_text: &BytesRef<Vec<u8>>) -> Result<String> {
        term_text.utf8_to_string()
    }

    /// Returns the text of this term. In the case of words, this is simply the
    /// text of the word. In the case of dates and other types, this is an
    /// encoding of the object as a string.
    pub fn text(&self) -> Result<String> {
        Self::get_string(&self.bytes)
    }
    /// Returns the bytes of this term, these should not be modified.
    pub fn bytes(&self) -> &BytesRef<Vec<u8>> {
        &self.bytes
    }
}
impl Display for Term {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}",
            self.field,
            self.text().unwrap_or_else(|_| "".to_string())
        )
    }
}
impl Accountable for Term {
    fn ram_bytes_used(&self) -> Result<i64> {
        todo!()
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
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.field.hash(state);
        self.bytes.hash(state);
    }
}

#[cfg(test)]
mod tests {
    use crate::core::index::term::Term;

    #[allow(dead_code)] // for quick search
    struct TestTerm;
    #[test]
    fn test_equals() {
        let base = Term::from_text("same", "same");
        let same = Term::from_text("same", "same");
        let different_field = Term::from_text("different", "same");
        let different_text = Term::from_text("same", "different");
        assert_eq!(base, base);
        assert_eq!(base, same);
        assert_ne!(base, different_field);
        assert_ne!(base, different_text);
    }
}
