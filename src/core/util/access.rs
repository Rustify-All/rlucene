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
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use parking_lot::Mutex;
use std::borrow::Cow;
use std::rc::Rc;
use std::sync::Arc;
/// A small abstraction that unifies access to an inner value `T`,
/// regardless of whether it is:
///
/// - owned directly (`T`)
/// - or shared behind synchronization (`Arc<Mutex<T>>`)
///
/// The key idea:
/// - `with_ref`  provides shared (read-only) access
/// - `with_mut`  provides exclusive (mutable) access
///
/// In the owned case, these are zero-cost abstractions.
/// In the shared case, they correspond to locking a mutex for the
/// duration of the closure.
pub trait MutAccess<T> {
  fn with_mut<R, F>(&mut self, f: F) -> R
  where
    F: FnOnce(&mut T) -> R;
  fn with_ref<R, F>(&self, f: F) -> R
  where
    F: FnOnce(&T) -> R;
}

impl<T> MutAccess<T> for T {
  #[inline]
  fn with_mut<R, F>(&mut self, f: F) -> R
  where
    F: FnOnce(&mut T) -> R,
  {
    f(self)
  }
  #[inline]
  fn with_ref<R, F>(&self, f: F) -> R
  where
    F: FnOnce(&T) -> R,
  {
    f(self)
  }
}

impl<T> MutAccess<T> for Arc<Mutex<T>> {
  #[inline]
  fn with_mut<R, F>(&mut self, f: F) -> R
  where
    F: FnOnce(&mut T) -> R,
  {
    let mut guard = self.lock();
    f(&mut *guard)
  }
  #[inline]
  fn with_ref<R, F>(&self, f: F) -> R
  where
    F: FnOnce(&T) -> R,
  {
    let guard = self.lock();
    f(&*guard)
  }
}
pub trait WritableVec<T>: SharedAccessVec<T> {
  fn access_mut<F, R>(&mut self, f: F) -> R
  where
    F: FnOnce(&mut Vec<T>) -> R;

  fn copy(&mut self, src: &[T], offset: usize);
}
impl<T> WritableVec<T> for Vec<T>
where
  T: Clone,
{
  fn access_mut<F, R>(&mut self, f: F) -> R
  where
    F: FnOnce(&mut Vec<T>) -> R,
  {
    f(self)
  }

  fn copy(&mut self, src: &[T], offset: usize) {
    self[offset..][..src.len()].clone_from_slice(src)
  }
}

pub trait SharedAccessVec<T>: Clone + Default {
  fn access<F, R>(&self, f: F) -> R
  where
    F: FnOnce(&Vec<T>) -> R;
  fn slice_clone(&self, offset: usize, length: usize) -> Self;
  fn new() -> Self;
  /// Constructs a buffer of `length` initialized elements.
  fn with_len<F>(length: usize, init: F) -> Result<Self>
  where
    F: FnMut() -> T;
  fn from_vec(v: Vec<T>) -> Self;
}

impl<T: Clone> SharedAccessVec<T> for Cow<'_, Vec<T>> {
  fn access<F, R>(&self, f: F) -> R
  where
    F: FnOnce(&Vec<T>) -> R,
  {
    f(self.as_ref())
  }

  fn slice_clone(&self, offset: usize, length: usize) -> Self {
    Cow::Owned(ArrayUtil::copy_of_sub_array(
      self.as_ref(),
      offset,
      offset + length,
    ))
  }

  fn new() -> Self {
    Cow::Owned(Vec::new())
  }

  fn with_len<F>(length: usize, init: F) -> Result<Self>
  where
    F: FnMut() -> T,
  {
    let mut values = Vec::with_capacity(length);
    values.resize_with(length, init);
    Ok(Cow::Owned(values))
  }

  fn from_vec(v: Vec<T>) -> Self {
    Cow::Owned(v)
  }
}

// Vec<T>
impl<T> SharedAccessVec<T> for Vec<T>
where
  T: Clone,
{
  fn access<F, R>(&self, f: F) -> R
  where
    F: FnOnce(&Vec<T>) -> R,
  {
    f(self)
  }

  fn slice_clone(&self, offset: usize, length: usize) -> Self {
    ArrayUtil::copy_of_sub_array(self, offset, offset + length)
  }

  fn new() -> Self {
    Vec::new()
  }

  fn with_len<F>(length: usize, init: F) -> Result<Self>
  where
    F: FnMut() -> T,
  {
    let mut values = Vec::with_capacity(length);
    values.resize_with(length, init);
    Ok(values)
  }

  fn from_vec(v: Vec<T>) -> Self {
    v
  }
}
// Rc<Vec<T>>
impl<T> SharedAccessVec<T> for Rc<Vec<T>>
where
  T: Clone,
{
  fn access<F, R>(&self, f: F) -> R
  where
    F: FnOnce(&Vec<T>) -> R,
  {
    f(self)
  }

  fn slice_clone(&self, offset: usize, length: usize) -> Self {
    let slice = &self[offset..offset + length];
    Rc::new(slice.to_vec())
  }

  fn new() -> Self {
    Rc::new(Vec::new())
  }

  fn with_len<F>(_length: usize, _init: F) -> Result<Self>
  where
    F: FnMut() -> T,
  {
    Err(LuceneError::unsupported_operation(""))
  }

  fn from_vec(v: Vec<T>) -> Self {
    Rc::new(v)
  }
}
// Arc<Vec<T>>
impl<T> SharedAccessVec<T> for Arc<Vec<T>>
where
  T: Clone,
{
  fn access<F, R>(&self, f: F) -> R
  where
    F: FnOnce(&Vec<T>) -> R,
  {
    f(self)
  }

  fn slice_clone(&self, offset: usize, length: usize) -> Self {
    let slice = &self[offset..offset + length];
    Arc::new(slice.to_vec())
  }

  fn new() -> Self {
    Arc::new(Vec::new())
  }

  fn with_len<F>(_length: usize, _init: F) -> Result<Self>
  where
    F: FnMut() -> T,
  {
    Err(LuceneError::unsupported_operation(""))
  }

  fn from_vec(v: Vec<T>) -> Self {
    Arc::new(v)
  }
}

#[macro_export]
macro_rules! with_other {
  (mut, $x:expr_2021, $y:expr_2021, |$ia:ident, $ib:ident| $body:expr_2021) => {
    $x.access_mut(|$ia| $y.access(|$ib| $body))
  };
  ($x:expr_2021, $y:expr_2021, |$ia:ident, $ib:ident| $body:expr_2021) => {
    $x.access(|$ia| $y.access(|$ib| $body))
  };
}
pub trait ByteSource: Default {
  fn as_slice(&self) -> &[u8];
}

impl ByteSource for Cow<'_, Vec<u8>> {
  fn as_slice(&self) -> &[u8] {
    self.as_ref().as_slice()
  }
}
impl ByteSource for Vec<u8> {
  fn as_slice(&self) -> &[u8] {
    self
  }
}

impl<const N: usize> ByteSource for [u8; N]
where
  [u8; N]: Default,
{
  fn as_slice(&self) -> &[u8] {
    self
  }
}

impl ByteSource for Rc<Vec<u8>> {
  fn as_slice(&self) -> &[u8] {
    self
  }
}

impl ByteSource for Arc<Vec<u8>> {
  fn as_slice(&self) -> &[u8] {
    self
  }
}

impl ByteSource for &[u8] {
  fn as_slice(&self) -> &[u8] {
    self
  }
}
pub trait ByteSourceMut {
  fn as_slice_mut(&mut self) -> &mut [u8];
  fn as_slice(&self) -> &[u8];
}
impl ByteSourceMut for Vec<u8> {
  fn as_slice_mut(&mut self) -> &mut [u8] {
    self
  }

  fn as_slice(&self) -> &[u8] {
    self
  }
}
impl ByteSourceMut for &mut [u8] {
  fn as_slice_mut(&mut self) -> &mut [u8] {
    self
  }

  fn as_slice(&self) -> &[u8] {
    self
  }
}
