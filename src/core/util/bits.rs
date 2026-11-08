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
use crate::core::index::index_reader::Identity;
use crate::core::util::HasIdentity;
use crate::core::util::bit_set::BitSet;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::fixed_bit_set::FixedBitSet;
use std::convert::Infallible;
use std::sync::Arc;

/// Trait for [`BitSet`]-like structures.
///
/// # Note
/// This is an experimental API.
pub trait Bits: HasIdentity {
  /// Returns the value of the bit at the specified `index`.
  ///
  /// # Arguments
  /// * `index` - The index should be non-negative and less than the length of
  ///   the bitset.
  ///
  /// # Returns
  /// `Ok(true)` if the bit is set, `Ok(false)` otherwise.
  fn get(&self, index: usize) -> Result<bool>;

  /// Returns the number of bits in this set
  fn length(&self) -> usize;

  /// Returns the backing fixed bit set when it represents this view exactly.
  fn fixed_bit_set(&self) -> Option<&FixedBitSet> {
    None
  }

  /// Make a copy of the given bits.
  fn copy_of(&self) -> Result<FixedBitSet> {
    let length = self.length();
    let mut bit_set = FixedBitSet::new(length);
    bit_set.set_with_range(0, length);
    for i in 0..length {
      if !self.get(i)? {
        bit_set.clear_with_index(i)?;
      }
    }
    Ok(bit_set)
  }
  fn to_string(&self) -> String {
    std::any::type_name::<Self>().to_string()
  }
}

impl HasIdentity for Infallible {
  fn identity(&self) -> &Identity {
    match *self {}
  }
}

impl Bits for Infallible {
  fn get(&self, _index: usize) -> Result<bool> {
    match *self {}
  }

  fn length(&self) -> usize {
    match *self {}
  }
}

/// Bits impl of the specified length with all bits set.
#[derive(Clone)]
pub struct MatchAllBits {
  len: usize,
  id: Identity,
}
impl MatchAllBits {
  pub fn new(len: usize) -> MatchAllBits {
    MatchAllBits {
      len,
      id: Identity::new(),
    }
  }
}

impl HasIdentity for MatchAllBits {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl Bits for MatchAllBits {
  fn get(&self, _index: usize) -> Result<bool> {
    Ok(true)
  }

  fn length(&self) -> usize {
    self.len
  }
}

/// Bits impl of the specified length with no bits set.
#[derive(Default, Clone)]
pub struct MatchNoBits {
  len: usize,
  id: Identity,
}
impl MatchNoBits {
  pub fn new(len: usize) -> Self {
    Self {
      len,
      id: Identity::new(),
    }
  }
}
impl HasIdentity for MatchNoBits {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl Bits for MatchNoBits {
  fn get(&self, _index: usize) -> Result<bool> {
    Ok(false)
  }

  fn length(&self) -> usize {
    self.len
  }
}

pub enum BitsEnum2<A, B> {
  A(A),
  B(B),
}

impl<A, B> Clone for BitsEnum2<A, B>
where
  A: Clone,
  B: Clone,
{
  fn clone(&self) -> Self {
    match self {
      BitsEnum2::A(t) => BitsEnum2::A(t.clone()),
      BitsEnum2::B(s) => BitsEnum2::B(s.clone()),
    }
  }
}

impl<A, B> HasIdentity for BitsEnum2<A, B>
where
  A: HasIdentity,
  B: HasIdentity,
{
  fn identity(&self) -> &Identity {
    match self {
      BitsEnum2::A(t) => t.identity(),
      BitsEnum2::B(s) => s.identity(),
    }
  }
}

impl<A, B> Bits for BitsEnum2<A, B>
where
  A: Bits,
  B: Bits,
{
  fn get(&self, index: usize) -> Result<bool> {
    match self {
      BitsEnum2::A(t) => t.get(index),
      BitsEnum2::B(s) => s.get(index),
    }
  }

  fn length(&self) -> usize {
    match self {
      BitsEnum2::A(t) => t.length(),
      BitsEnum2::B(s) => s.length(),
    }
  }

  fn copy_of(&self) -> Result<FixedBitSet> {
    match self {
      BitsEnum2::A(t) => t.copy_of(),
      BitsEnum2::B(s) => s.copy_of(),
    }
  }

  fn to_string(&self) -> String {
    match self {
      BitsEnum2::A(t) => t.to_string(),
      BitsEnum2::B(s) => s.to_string(),
    }
  }
}

impl<T> Bits for Arc<T>
where
  T: Bits + ?Sized,
{
  fn get(&self, index: usize) -> Result<bool> {
    (**self).get(index)
  }

  fn length(&self) -> usize {
    (**self).length()
  }

  fn fixed_bit_set(&self) -> Option<&FixedBitSet> {
    (**self).fixed_bit_set()
  }

  fn copy_of(&self) -> Result<FixedBitSet> {
    (**self).copy_of()
  }
  fn to_string(&self) -> String {
    (**self).to_string()
  }
}

impl<T> Bits for &T
where
  T: Bits + ?Sized,
{
  fn get(&self, index: usize) -> Result<bool> {
    <T as Bits>::get(*self, index)
  }

  fn length(&self) -> usize {
    <T as Bits>::length(*self)
  }

  fn fixed_bit_set(&self) -> Option<&FixedBitSet> {
    <T as Bits>::fixed_bit_set(*self)
  }

  fn copy_of(&self) -> Result<FixedBitSet> {
    <T as Bits>::copy_of(*self)
  }

  fn to_string(&self) -> String {
    <T as Bits>::to_string(*self)
  }
}
