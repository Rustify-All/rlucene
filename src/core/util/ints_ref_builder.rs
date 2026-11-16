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
use crate::core::util::CoreHelper;
use crate::core::util::access::{SharedAccessVec, WritableVec};
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::ints_ref::IntsRef;

/// A builder for [`IntsRef`] instances.
///
/// Internal utility used during FST construction.
///
/// # Lucene internal
#[derive(Debug)]
pub struct IntsRefBuilder<AV> {
  ints_ref: IntsRef<AV>,
}

impl<AV> Clone for IntsRefBuilder<AV>
where
  AV: Clone,
{
  fn clone(&self) -> Self {
    Self {
      ints_ref: self.ints_ref.clone(),
    }
  }
}

impl<AV> Default for IntsRefBuilder<AV>
where
  AV: WritableVec<i32>,
{
  fn default() -> Self {
    Self::new()
  }
}

impl<AV> IntsRefBuilder<AV>
where
  AV: SharedAccessVec<i32>,
{
  pub fn new() -> Self
  where
    AV: WritableVec<i32>,
  {
    Self {
      ints_ref: IntsRef::default(),
    }
  }

  /// Returns the underlying `i32` buffer.
  pub fn ints(&mut self) -> &mut AV {
    &mut self.ints_ref.ints
  }

  /// Returns the number of `i32` values in this buffer.
  pub fn length(&self) -> usize {
    self.ints_ref.length
  }

  /// Sets the current length of the buffer.
  pub fn set_length(&mut self, length: usize) {
    self.ints_ref.length = length;
  }

  /// Empties this builder.
  pub fn clear(&mut self) {
    self.set_length(0);
  }
  /// Returns the `i32` at the given offset.
  pub fn int_at(&self, offset: usize) -> i32 {
    self.ints_ref.ints.access(|ints_bytes| ints_bytes[offset])
  }

  /// Sets the `i32` at the given offset.
  pub fn set_int_at(&mut self, offset: usize, value: i32)
  where
    AV: WritableVec<i32>,
  {
    self.ints_ref.ints.access_mut(|ints_bytes| {
      ints_bytes[offset] = value;
    })
  }

  /// Appends the provided `i32` to this buffer.
  pub fn append(&mut self, i: i32) -> Result<()>
  where
    AV: WritableVec<i32>,
  {
    let mut len = self.ints_ref.length;
    self.grow(len + 1)?;
    len = self.ints_ref.length;
    self.ints_ref.ints.access_mut(|ints_bytes| {
      ints_bytes[len] = i;
    });
    self.ints_ref.length += 1;
    Ok(())
  }

  /// Grows the reference array to at least `new_length`.
  ///
  /// In general, this should not be used directly, as it does not take offset
  /// into account.
  pub fn grow(&mut self, new_length: usize) -> Result<()>
  where
    AV: WritableVec<i32>,
  {
    self
      .ints_ref
      .ints
      .access_mut(|ints_bytes| ArrayUtil::grow_with(ints_bytes, new_length, || 0))
  }

  /// Grows the reference array to at least `new_length`, without copying
  /// original data.
  pub fn grow_no_copy(&mut self, new_length: usize) -> Result<()>
  where
    AV: WritableVec<i32>,
  {
    self
      .ints_ref
      .ints
      .access_mut(|ints_bytes| ArrayUtil::grow_no_copy(ints_bytes, new_length, || 0))
  }

  /// Copies the given slice into this instance.
  pub fn copy_ints(&mut self, other: &[i32], other_offset: usize, other_length: usize) -> Result<()>
  where
    AV: WritableVec<i32>,
  {
    self.ints_ref.ints.access_mut(|ints_bytes| {
      ArrayUtil::grow_no_copy(ints_bytes, other_length, || 0)?;
      CoreHelper::check_from_index_size(other_offset, other_length, other.len())?;
      let copy_source = &other[other_offset..(other_offset + other_length)];
      ints_bytes[..copy_source.len()].copy_from_slice(copy_source);
      self.ints_ref.length = other_length;
      Ok(())
    })
  }
  /// Copies the given [`IntsRef`] into this instance.
  pub fn copy_ints_ref(&mut self, ints: &IntsRef<AV>) -> Result<()>
  where
    AV: WritableVec<i32>,
  {
    ints
      .ints
      .access(|ints_bytes| self.copy_ints(ints_bytes, ints.offset, ints.length))
  }

  /// Returns a reference to the internal [`IntsRef`] content.
  ///
  /// Any modification to this builder may invalidate the returned value.
  pub fn get(&self) -> &IntsRef<AV> {
    debug_assert_eq!(
      self.ints_ref.offset, 0,
      "Modifying the offset of the returned ref is illegal"
    );
    &self.ints_ref
  }
  pub fn get_owner(&mut self) -> IntsRef<AV>
  where
    AV: WritableVec<i32>,
  {
    std::mem::take(&mut self.ints_ref)
  }

  /// Builds a new [`IntsRef`] that has the same content as this builder.
  pub fn to_ints_ref(&self) -> IntsRef<AV> {
    IntsRef::deep_copy_of(&self.ints_ref)
  }
}
