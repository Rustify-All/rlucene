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
use crate::core::util::access::{ByteSource, SharedAccessVec};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::{CoreHelper, GOOD_FAST_HASH_SEED, HashCode, StringHelper};
use crate::with_other;
use std::borrow::Cow;
use std::cmp::Ordering;
use std::fmt::{Debug, Display};
use std::hash::Hash;
use std::sync::Arc;

/// Represents a `&[u8]` as a slice (offset + length) into an existing byte
/// array. The `bytes` member should never be `None`;
///
/// # Important Note
/// To convert them to a Rust `String` (which is UTF-8), use `utf8_to_string`.
/// Using code like `String::from_utf8_lossy(&bytes[offset.offset+length])` is
/// the correct way to handle this. Avoid constructing strings incorrectly, as
/// it may result in wrong results.
///
/// # Sorting
/// This struct implements `Ord`. The underlying byte arrays are sorted
/// lexicographically, treating elements as unsigned. This is identical to
/// Unicode codepoint order.
#[derive(Debug, Default)]
pub struct BytesRef<AV> {
  /// The contents of the BytesRef
  pub bytes: AV,
  pub offset: usize,
  pub length: usize,
}

impl<AV> BytesRef<AV>
where
  AV: ByteSource,
{
  /// Returns the active byte range represented by this value.
  #[inline]
  pub fn as_byte_slice(&self) -> &[u8] {
    &self.bytes.as_slice()[self.offset..self.offset + self.length]
  }

  /// Compares this value with another BytesRef independently of their
  /// backing byte containers.
  #[inline]
  pub fn compare_to<B>(&self, other: &BytesRef<B>) -> Ordering
  where
    B: ByteSource,
  {
    self.as_byte_slice().cmp(other.as_byte_slice())
  }

  /// Performs internal consistency checks on the active byte range.
  pub fn is_valid(&self) -> Result<bool> {
    let bytes = self.bytes.as_slice();
    if self.length > bytes.len() {
      return Err(LuceneError::illegal_state(format!(
        "length is out of bounds: {},bytes.length= {}",
        self.length,
        bytes.len()
      )));
    }
    if self.offset > bytes.len() {
      return Err(LuceneError::illegal_state(format!(
        "offset out of bounds: {},bytes.length= {}",
        self.offset,
        bytes.len()
      )));
    }
    if self.offset + self.length > bytes.len() {
      return Err(LuceneError::illegal_state(format!(
        "offset+length out of bounds: offset={},length={},bytes.length= {}",
        self.offset,
        self.length,
        bytes.len()
      )));
    }
    Ok(true)
  }
}

impl BytesRef<Vec<u8>> {
  /// Replaces the contents with a copy of `bytes`, reusing the existing allocation
  /// when its capacity is sufficient. Resets the offset to zero.
  pub fn copy_from_slice(&mut self, bytes: &[u8]) {
    self.bytes.clear();
    self.bytes.extend_from_slice(bytes);
    self.offset = 0;
    self.length = bytes.len();
  }
}

impl BytesRef<Arc<Vec<u8>>> {
  /// compare: same bytes reference, same offset, same length
  pub fn equals(a: &BytesRef<Arc<Vec<u8>>>, b: &BytesRef<Arc<Vec<u8>>>) -> bool {
    let v = Arc::ptr_eq(&a.bytes, &b.bytes);
    // Check reference equality: if the bytes reference is the same,
    // then offset and length must also be equal.
    debug_assert!({
      if v {
        a.offset == b.offset && a.length == b.length
      } else {
        !v
      }
    });
    v
  }
}

impl<AV> BytesRef<AV>
where
  AV: SharedAccessVec<u8>,
{
  pub fn new() -> Self {
    BytesRef {
      bytes: AV::new(),
      offset: 0,
      length: 0,
    }
  }

  pub fn with_capacity(capacity: usize) -> Result<Self> {
    Ok(BytesRef {
      bytes: AV::from_vec(vec![0; capacity]),
      offset: 0,
      length: 0,
    })
  }
  pub fn from_slice(bytes: AV, offset: usize, length: usize) -> Self {
    let bytes_ref = BytesRef {
      bytes,
      offset,
      length,
    };
    debug_assert!(bytes_ref.bytes.access(|bytes| {
      bytes_ref.offset <= bytes.len()
        && bytes_ref.length <= bytes.len()
        && bytes_ref.offset + bytes_ref.length <= bytes.len()
    }));
    bytes_ref
  }
  /// This instance will directly share/ownership bytes w/o making a copy
  pub fn from_bytes(bytes: AV) -> Self {
    let len = bytes.access(|bytes| bytes.len());
    BytesRef {
      bytes,
      offset: 0,
      length: len,
    }
  }
  /// Initialize the byte container from UTF-8 text.
  pub fn from_string(s: &str) -> Self {
    let container = AV::from_vec(s.as_bytes().to_vec());
    let len = s.len();
    BytesRef {
      bytes: container,
      offset: 0,
      length: len,
    }
  }
  /// Expert: compares the bytes against another BytesRef, returning true if
  /// the bytes are equal.
  ///
  /// # Arguments
  /// * `other` - Another BytesRef
  pub fn bytes_equals(&self, other: &BytesRef<AV>) -> bool {
    with_other!(self.bytes, other.bytes, |ints_bytes, other_bytes| {
      let self_slice = &ints_bytes[self.offset..(self.offset + self.length)];
      let other_slice = &other_bytes[other.offset..(other.offset + other.length)];
      self_slice == other_slice
    })
  }
  /// Interprets the stored bytes as UTF-8, returning the resulting string.
  pub fn utf8_to_string(&self) -> Result<String> {
    self.bytes.access(|bytes| {
      CoreHelper::check_from_index_size(self.offset, self.length, bytes.len())?;
      let slice = &bytes[self.offset..self.offset + self.length];
      std::str::from_utf8(slice)
        .map(|s| s.to_owned())
        .map_err(LuceneError::from)
    })
  }
  pub fn deep_copy_of(other: &BytesRef<AV>) -> Result<Self> {
    other.bytes.access(|bytes| {
      CoreHelper::check_from_index_size(other.offset, other.length, bytes.len())?;
      let slice = &bytes[other.offset..other.offset + other.length];
      Ok(BytesRef::from_bytes(AV::from_vec(slice.to_vec())))
    })
  }
  pub fn take_bytes(&mut self) -> AV {
    std::mem::take(&mut self.bytes)
  }
}
impl<AV> PartialOrd for BytesRef<AV>
where
  AV: ByteSource,
{
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    Some(self.cmp(other))
  }
}

impl<AV> Eq for BytesRef<AV> where AV: ByteSource {}

impl<AV> Ord for BytesRef<AV>
where
  AV: ByteSource,
{
  fn cmp(&self, other: &Self) -> Ordering {
    self.compare_to(other)
  }
}

impl<AV> Clone for BytesRef<AV>
where
  AV: SharedAccessVec<u8>,
{
  fn clone(&self) -> Self {
    BytesRef::from_slice(self.bytes.clone(), self.offset, self.length)
  }
}
impl<AV> Hash for BytesRef<AV>
where
  AV: SharedAccessVec<u8>,
{
  fn hash<H>(&self, state: &mut H)
  where
    H: std::hash::Hasher,
  {
    let hash = StringHelper::murmurhash3_x86_32(self, *GOOD_FAST_HASH_SEED);
    hash.hash(state)
  }
}
impl<AV> PartialEq for BytesRef<AV>
where
  AV: ByteSource,
{
  fn eq(&self, other: &Self) -> bool {
    self.as_byte_slice() == other.as_byte_slice()
  }
}

impl<AV> Display for BytesRef<AV>
where
  AV: ByteSource,
{
  fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
    let bytes = self.bytes.as_slice();
    write!(f, "[")?;
    let end = self.offset + self.length;

    for (i, &byte) in bytes[self.offset..end].iter().enumerate() {
      if i > 0 {
        write!(f, " ")?;
      }
      write!(f, "{byte:02x}")?;
    }
    write!(f, "]")?;
    Ok(())
  }
}
impl<AV> HashCode for BytesRef<AV>
where
  AV: SharedAccessVec<u8>,
{
  fn hash_code(&self) -> i32 {
    StringHelper::murmurhash3_x86_32(self, *GOOD_FAST_HASH_SEED)
  }
}
impl From<String> for BytesRef<Vec<u8>> {
  fn from(value: String) -> Self {
    BytesRef::from_bytes(value.into_bytes())
  }
}
impl From<&str> for BytesRef<Vec<u8>> {
  fn from(value: &str) -> Self {
    BytesRef::from_string(value)
  }
}

impl From<&String> for BytesRef<Vec<u8>> {
  fn from(value: &String) -> Self {
    BytesRef::from_string(value)
  }
}

impl From<Vec<u8>> for BytesRef<Vec<u8>> {
  fn from(value: Vec<u8>) -> Self {
    BytesRef::from_bytes(value)
  }
}

impl From<&[u8]> for BytesRef<Vec<u8>> {
  fn from(value: &[u8]) -> Self {
    BytesRef::from_bytes(value.to_vec())
  }
}

impl From<&Vec<u8>> for BytesRef<Vec<u8>> {
  fn from(value: &Vec<u8>) -> Self {
    BytesRef::from(value.as_slice())
  }
}

impl<const N: usize> From<[u8; N]> for BytesRef<Vec<u8>> {
  fn from(value: [u8; N]) -> Self {
    BytesRef::from_bytes(Vec::from(value))
  }
}

impl<const N: usize> From<&[u8; N]> for BytesRef<Vec<u8>> {
  fn from(value: &[u8; N]) -> Self {
    BytesRef::from(value.as_slice())
  }
}

/// A byte value that can expose its contents without taking ownership.
/// The lifetime is that of its owner borrow, including through enum adapters.
pub trait BytesRefValue<'a>: Sized + Debug + Display {
  fn as_bytes_ref(&self) -> BytesRef<&[u8]>;

  /// Normalize heterogeneous results without copying their byte storage.
  fn into_value(self) -> BytesRefValueEnum<'a>;

  fn as_bytes(&self) -> &[u8] {
    let value = self.as_bytes_ref();
    &value.bytes[value.offset..value.offset + value.length]
  }

  /// Move owned storage, or copy a borrowed value when it must outlive the lookup.
  #[inline]
  fn into_owned(self) -> BytesRef<Vec<u8>> {
    match self.into_value() {
      BytesRefValueEnum::Buffer(value) => value.into_owned(),
      BytesRefValueEnum::Slice(value) => BytesRef::from(value.as_bytes().to_vec()),
    }
  }

  fn is_valid(&self) -> Result<bool> {
    let value = self.as_bytes_ref();
    if value.length > value.bytes.len() {
      return Err(LuceneError::illegal_state(format!(
        "length is out of bounds: {},bytes.length= {}",
        value.length,
        value.bytes.len()
      )));
    }
    if value.offset > value.bytes.len() {
      return Err(LuceneError::illegal_state(format!(
        "offset out of bounds: {},bytes.length= {}",
        value.offset,
        value.bytes.len()
      )));
    }
    if value.offset + value.length > value.bytes.len() {
      return Err(LuceneError::illegal_state(format!(
        "offset+length out of bounds: offset={},length={},bytes.length= {}",
        value.offset,
        value.length,
        value.bytes.len()
      )));
    }
    Ok(true)
  }

  fn utf8_to_string(&self) -> Result<&str> {
    let value = self.as_bytes_ref();
    CoreHelper::check_from_index_size(value.offset, value.length, value.bytes.len())?;
    std::str::from_utf8(&value.bytes[value.offset..value.offset + value.length])
      .map_err(LuceneError::from)
  }
}

/// A bounded carrier for enum adapters whose variants return different byte storage.
/// Buffer-backed results retain their existing borrowing/ownership behavior.
#[derive(Debug, PartialEq, Eq)]
pub enum BytesRefValueEnum<'a> {
  Buffer(Cow<'a, BytesRef<Vec<u8>>>),
  Slice(BytesRef<&'a [u8]>),
}

/// Preserves the concrete byte carriers of a two-variant adapter.
///
/// Unlike [`BytesRefValueEnum`], this type does not widen borrowed results to
/// the largest supported carrier unless the caller explicitly requests an
/// owned or normalized value.
#[derive(Debug, PartialEq, Eq)]
pub enum BytesRefValueEnum2<A, B> {
  A(A),
  B(B),
}

impl<A, B> Display for BytesRefValueEnum2<A, B>
where
  A: Debug + Display,
  B: Debug + Display,
{
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::A(value) => Display::fmt(value, f),
      Self::B(value) => Display::fmt(value, f),
    }
  }
}

impl<'a, A, B> BytesRefValue<'a> for BytesRefValueEnum2<A, B>
where
  A: BytesRefValue<'a>,
  B: BytesRefValue<'a>,
{
  #[inline(always)]
  fn as_bytes_ref(&self) -> BytesRef<&[u8]> {
    match self {
      Self::A(value) => value.as_bytes_ref(),
      Self::B(value) => value.as_bytes_ref(),
    }
  }

  fn into_value(self) -> BytesRefValueEnum<'a> {
    match self {
      Self::A(value) => value.into_value(),
      Self::B(value) => value.into_value(),
    }
  }
}

impl Display for BytesRefValueEnum<'_> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    Display::fmt(&self.as_bytes_ref(), f)
  }
}

impl<'a> BytesRefValueEnum<'a> {
  /// Moves owned byte storage into the target, or copies borrowed bytes into it.
  pub(crate) fn copy_or_move_into(self, target: &mut BytesRef<Vec<u8>>) {
    match self {
      Self::Buffer(Cow::Owned(value)) => *target = value,
      value => target.copy_from_slice(value.as_bytes()),
    }
  }

  /// Adapt to an API requiring Vec-backed BytesRef, copying only a slice-backed value.
  pub fn into_cow(self) -> Cow<'a, BytesRef<Vec<u8>>> {
    match self {
      Self::Buffer(value) => value,
      Self::Slice(value) => Cow::Owned(BytesRef::from(value.as_bytes().to_vec())),
    }
  }
}

impl<'a> BytesRefValue<'a> for Cow<'a, BytesRef<Vec<u8>>> {
  #[inline(always)]
  fn as_bytes_ref(&self) -> BytesRef<&[u8]> {
    BytesRef {
      bytes: &self.bytes,
      offset: self.offset,
      length: self.length,
    }
  }

  fn into_value(self) -> BytesRefValueEnum<'a> {
    BytesRefValueEnum::Buffer(self)
  }
}

impl<'a> BytesRefValue<'a> for &'a BytesRef<Vec<u8>> {
  #[inline(always)]
  fn as_bytes_ref(&self) -> BytesRef<&[u8]> {
    BytesRef {
      bytes: &self.bytes,
      offset: self.offset,
      length: self.length,
    }
  }

  fn into_value(self) -> BytesRefValueEnum<'a> {
    BytesRefValueEnum::Buffer(Cow::Borrowed(self))
  }
}

impl<'a> BytesRefValue<'a> for BytesRef<&'a [u8]> {
  #[inline(always)]
  fn as_bytes_ref(&self) -> BytesRef<&[u8]> {
    BytesRef {
      bytes: self.bytes,
      offset: self.offset,
      length: self.length,
    }
  }

  fn into_value(self) -> BytesRefValueEnum<'a> {
    BytesRefValueEnum::Slice(self)
  }
}

impl<'a, const N: usize> BytesRefValue<'a> for &'a BytesRef<[u8; N]>
where
  [u8; N]: Default,
{
  #[inline(always)]
  fn as_bytes_ref(&self) -> BytesRef<&[u8]> {
    BytesRef {
      bytes: &self.bytes,
      offset: self.offset,
      length: self.length,
    }
  }

  fn into_value(self) -> BytesRefValueEnum<'a> {
    BytesRefValueEnum::Slice(BytesRef {
      bytes: &self.bytes,
      offset: self.offset,
      length: self.length,
    })
  }
}

#[derive(Debug)]
pub struct BytesRefCow<'a>(pub Cow<'a, [u8]>);

impl Display for BytesRefCow<'_> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    Display::fmt(&self.as_bytes_ref(), f)
  }
}

impl<'a> BytesRefValue<'a> for BytesRefCow<'a> {
  fn as_bytes_ref(&self) -> BytesRef<&[u8]> {
    BytesRef {
      bytes: self.0.as_ref(),
      offset: 0,
      length: self.0.len(),
    }
  }

  fn into_value(self) -> BytesRefValueEnum<'a> {
    match self.0 {
      Cow::Borrowed(bytes) => BytesRefValueEnum::Slice(BytesRef {
        bytes,
        offset: 0,
        length: bytes.len(),
      }),
      Cow::Owned(bytes) => BytesRefValueEnum::Buffer(Cow::Owned(BytesRef::from(bytes))),
    }
  }
}

impl<'a> BytesRefValue<'a> for BytesRefValueEnum<'a> {
  #[inline(always)]
  fn as_bytes_ref(&self) -> BytesRef<&[u8]> {
    match self {
      Self::Buffer(value) => value.as_bytes_ref(),
      Self::Slice(value) => value.as_bytes_ref(),
    }
  }

  fn into_value(self) -> Self {
    self
  }
}

impl<'a> BytesRefValue<'a> for BytesRef<Vec<u8>> {
  #[inline(always)]
  fn as_bytes_ref(&self) -> BytesRef<&[u8]> {
    BytesRef {
      bytes: &self.bytes,
      offset: self.offset,
      length: self.length,
    }
  }

  fn into_value(self) -> BytesRefValueEnum<'a> {
    BytesRefValueEnum::Buffer(Cow::Owned(self))
  }
}
