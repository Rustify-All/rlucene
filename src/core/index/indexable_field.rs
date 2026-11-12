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

use crate::core::analysis::analyzer::Analyzer;
use crate::core::analysis::reader::ReaderEnum;
use crate::core::analysis::token_stream::{AnalyzerTokenStreams, TokenStreamEnum2};
use crate::core::document::field::{BinaryTokenStream, StringTokenStream};
use crate::core::document::field::{FieldDataEnum, IndexingTokenStreamEnum3};
use crate::core::document::fields::FieldTokenStreamEnum;
use crate::core::document::invertable_field::InvertableType;
use crate::core::index::indexable_field_type::IndexableFieldType;
use crate::core::index::{BytesRef, BytesRefValue, BytesRefValueEnum};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::number::Number;
use std::borrow::{Borrow, Cow};
use std::cell::RefMut;
use std::fmt::Display;

/// Represents a single field for indexing. [`IndexWriter`](crate::core::index::index_writer::IndexWriter)
/// consumes a fallible iterator of [`Fields`](crate::core::document::fields::Fields) as a document.
pub trait IndexableField: Display {
  /// Field name
  fn name(&self) -> &str;

  /// [`IndexableFieldType`] describing the properties of this field.
  type FieldType<'a>: IndexableFieldType
  where
    Self: 'a;
  fn field_type(&self) -> Self::FieldType<'_>;
  /// Creates the TokenStream used for indexing this field. If appropriate,
  /// implementations should use the given Analyzer to create the
  /// TokenStreams.
  ///
  /// * `analyzer` - Analyzer that should be used to create the TokenStreams
  ///   from
  /// * `reuse` - TokenStream for a previous instance of this field **name**.
  ///   This allows custom field types (like StringField and NumericField)
  ///   that do not use the analyzer to still have good performance. Note: the
  ///   passed-in type may be inappropriate, for example if you mix up
  ///   different types of Fields for the same field name. So it's the
  ///   responsibility of the implementation to check.
  ///
  /// # Returns
  /// TokenStream value for indexing the document. Should always return a
  /// present value if the field is to be indexed.
  fn token_stream<'a, A>(
    &'a mut self,
    analyzer: &'a A,
    reuse_token_stream: &'a mut Option<ReusedIndexingTokenStream>,
  ) -> Result<IndexingTokenStream<'a>>
  where
    A: Analyzer;
  /// present if this field has a binary value.
  fn binary_value(&self) -> Result<Option<BinaryValueEnum<'_>>>;
  fn take_binary_value(&mut self) -> Result<Option<BytesRef<Vec<u8>>>>;

  /// present if this field has a string value.
  fn string_value(&self) -> Result<Option<Cow<'_, String>>>;
  fn take_string_value(&mut self) -> Result<Option<String>>;
  /// present if this field has a string value.
  fn get_char_sequence_value(&self) -> Result<Option<Cow<'_, String>>> {
    self.string_value()
  }

  /// present if this field has a Reader value.
  fn take_reader_value(&mut self) -> Result<Option<ReaderEnum>>;

  /// present if this field has a numeric value.
  fn numeric_value(&self) -> Result<Option<Number>>;

  /// Storage returned for a stored field, either borrowed or independently owned.
  type StoredValue<'a>: Borrow<FieldDataEnum>
  where
    Self: 'a;

  /// Stored value. This method is called to populate stored fields and must
  /// return a present value if the field is stored.
  fn stored_value(&self) -> Result<Option<Self::StoredValue<'_>>>;

  /// Describes how this field should be inverted. This must return a present
  /// value if the field indexes terms and postings.
  fn invertable_type(&self) -> &InvertableType;

  fn is_reserved(&self) -> bool {
    false
  }

  /// Returns the floating-point vector without erasing its element type.
  fn float_vector_value(&self) -> Result<&[f32]> {
    Err(LuceneError::unsupported_operation(""))
  }

  /// Returns the byte vector without erasing its element type.
  fn byte_vector_value(&self) -> Result<&[u8]> {
    Err(LuceneError::unsupported_operation(""))
  }
}

/// Stored field data returned by an enum whose variants use different ownership.
pub enum StoredValueEnum<'a> {
  Borrowed(&'a FieldDataEnum),
  Owned(FieldDataEnum),
}

impl Borrow<FieldDataEnum> for StoredValueEnum<'_> {
  fn borrow(&self) -> &FieldDataEnum {
    match self {
      Self::Borrowed(value) => value,
      Self::Owned(value) => value,
    }
  }
}

impl<'a> From<&'a FieldDataEnum> for StoredValueEnum<'a> {
  fn from(value: &'a FieldDataEnum) -> Self {
    Self::Borrowed(value)
  }
}

impl From<FieldDataEnum> for StoredValueEnum<'_> {
  fn from(value: FieldDataEnum) -> Self {
    Self::Owned(value)
  }
}

pub type IndexingTokenStream<'a> = Option<
  IndexingTokenStreamEnum3<
    RefMut<'a, AnalyzerTokenStreams>,
    &'a mut ReusedIndexingTokenStream,
    &'a mut FieldTokenStreamEnum,
  >,
>;
pub type ReusedIndexingTokenStream = TokenStreamEnum2<BinaryTokenStream, StringTokenStream>;
/// Binary value returned by an indexable field.
///
/// Borrowed values reuse existing Vec-backed storage, slice values borrow external
/// bytes, and owned values avoid wrapping a newly allocated BytesRef in a Cow.
/// Inline values hold small encodings directly in the result.
#[derive(Debug, PartialEq, Eq)]
pub enum BinaryValueEnum<'a> {
  Borrowed(&'a BytesRef<Vec<u8>>),
  Owned(BytesRef<Vec<u8>>),
  Slice(BytesRef<&'a [u8]>),
  /// Small owned encodings. `length` must not exceed eight bytes.
  Inline {
    bytes: [u8; 8],
    length: usize,
  },
}

impl From<BytesRef<Vec<u8>>> for BinaryValueEnum<'_> {
  #[inline]
  fn from(value: BytesRef<Vec<u8>>) -> Self {
    Self::Owned(value)
  }
}

impl<'a> From<&'a BytesRef<Vec<u8>>> for BinaryValueEnum<'a> {
  #[inline]
  fn from(value: &'a BytesRef<Vec<u8>>) -> Self {
    Self::Borrowed(value)
  }
}

impl From<Vec<u8>> for BinaryValueEnum<'_> {
  #[inline]
  fn from(value: Vec<u8>) -> Self {
    let length = value.len();
    Self::Owned(BytesRef {
      bytes: value,
      offset: 0,
      length,
    })
  }
}

impl From<String> for BinaryValueEnum<'_> {
  #[inline]
  fn from(value: String) -> Self {
    Self::from(value.into_bytes())
  }
}

impl<'a> From<&'a [u8]> for BinaryValueEnum<'a> {
  #[inline]
  fn from(value: &'a [u8]) -> Self {
    Self::Slice(BytesRef {
      bytes: value,
      offset: 0,
      length: value.len(),
    })
  }
}

impl<'a> From<&'a Vec<u8>> for BinaryValueEnum<'a> {
  #[inline]
  fn from(value: &'a Vec<u8>) -> Self {
    Self::from(value.as_slice())
  }
}

impl<'a> From<&'a str> for BinaryValueEnum<'a> {
  #[inline]
  fn from(value: &'a str) -> Self {
    Self::from(value.as_bytes())
  }
}

impl<'a> From<&'a String> for BinaryValueEnum<'a> {
  #[inline]
  fn from(value: &'a String) -> Self {
    Self::from(value.as_bytes())
  }
}

impl<const N: usize> From<[u8; N]> for BinaryValueEnum<'_> {
  #[inline]
  fn from(value: [u8; N]) -> Self {
    Self::from(Vec::from(value))
  }
}

impl<'a, const N: usize> From<&'a [u8; N]> for BinaryValueEnum<'a> {
  #[inline]
  fn from(value: &'a [u8; N]) -> Self {
    Self::from(value.as_slice())
  }
}

impl<'a> BinaryValueEnum<'a> {
  /// Adapt to an API requiring Vec-backed BytesRef, copying slice and inline values.
  pub fn into_cow(self) -> Cow<'a, BytesRef<Vec<u8>>> {
    match self {
      Self::Borrowed(value) => Cow::Borrowed(value),
      Self::Owned(value) => Cow::Owned(value),
      Self::Slice(value) => Cow::Owned(BytesRef::from(value.as_bytes().to_vec())),
      Self::Inline { bytes, length } => Cow::Owned(BytesRef::from(bytes[..length].to_vec())),
    }
  }

  /// Move owned storage, or provide owned storage when the caller requires it.
  pub fn into_owned(self) -> BytesRef<Vec<u8>> {
    match self {
      Self::Borrowed(value) => value.clone(),
      Self::Owned(value) => value,
      Self::Slice(value) => BytesRef::from(value.as_bytes().to_vec()),
      Self::Inline { bytes, length } => BytesRef::from(bytes[..length].to_vec()),
    }
  }

  #[inline(always)]
  pub fn as_bytes_ref(&self) -> BytesRef<&[u8]> {
    match self {
      Self::Borrowed(value) => value.as_bytes_ref(),
      Self::Owned(value) => value.as_bytes_ref(),
      Self::Slice(value) => value.as_bytes_ref(),
      Self::Inline { bytes, length } => BytesRef {
        bytes,
        offset: 0,
        length: *length,
      },
    }
  }

  #[inline(always)]
  pub fn as_byte_slice(&self) -> &[u8] {
    match self {
      Self::Borrowed(value) => value.as_byte_slice(),
      Self::Owned(value) => value.as_byte_slice(),
      Self::Slice(value) => value.as_byte_slice(),
      Self::Inline { bytes, length } => &bytes[..*length],
    }
  }
}

impl Display for BinaryValueEnum<'_> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    Display::fmt(&self.as_bytes_ref(), f)
  }
}

impl<'a> BytesRefValue<'a> for BinaryValueEnum<'a> {
  #[inline(always)]
  fn as_bytes_ref(&self) -> BytesRef<&[u8]> {
    BinaryValueEnum::as_bytes_ref(self)
  }

  fn into_value(self) -> BytesRefValueEnum<'a> {
    match self {
      Self::Borrowed(value) => BytesRefValueEnum::Buffer(Cow::Borrowed(value)),
      Self::Owned(value) => BytesRefValueEnum::Buffer(Cow::Owned(value)),
      Self::Slice(value) => BytesRefValueEnum::Slice(value),
      Self::Inline { bytes, length } => {
        BytesRefValueEnum::Buffer(Cow::Owned(BytesRef::from(bytes[..length].to_vec())))
      },
    }
  }
}
