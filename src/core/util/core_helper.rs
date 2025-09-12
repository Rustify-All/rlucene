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
use std::cell::RefCell;
use std::cmp::Ordering;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::rc::Rc;

use bit_set::BitSet;

use crate::core::index::BytesRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::ints_ref::IntsRef;

pub struct CoreHelper;
impl CoreHelper {
    pub const CLONE_WARRING: &'static str = "does not implement the Clone logic.
The purpose of implementing the Clone trait is to make it could be used with Cow";
    pub fn check_from_index_size(from_index: i32, size: i32, length: i32) -> Result<i32> {
        if from_index < 0 || size < 0 || length < 0 {
            Err(LuceneError::array_index_out_of_bounds(format!(
                "from_index: {from_index}, size: {size}, and length {length} must be non-negative"
            )))
        } else if size > length - from_index {
            Err(LuceneError::array_index_out_of_bounds(format!(
                "size: {size} is too large, from_index: {from_index}, length: {length}"
            )))
        } else {
            Ok(from_index)
        }
    }
    pub fn check_from_to_index(from_index: usize, to_index: usize, length: usize) -> Result<usize> {
        if from_index > to_index || to_index > length {
            return Err(LuceneError::array_index_out_of_bounds(format!(
                "index out of bounds: from_index={from_index} to_index={to_index} length={length}"
            )));
        }
        Ok(from_index)
    }
    pub fn check_index(index: i32, length: i32) -> Result<i32> {
        if index < 0 || index >= length {
            return Err(LuceneError::array_index_out_of_bounds(format!(
                "index out of bounds: index={index} length={length}"
            )));
        }
        Ok(index)
    }
    pub fn miss_match<T: PartialEq>(a: &[T], b: &[T]) -> i32 {
        let miss_match = a.iter().zip(b.iter()).position(|(x, y)| x != y);
        match miss_match {
            Some(i) => i as i32,
            None => match a.len().cmp(&b.len()) {
                Ordering::Greater => b.len() as i32,
                Ordering::Less => a.len() as i32,
                Ordering::Equal => -1,
            },
        }
    }

    pub fn take_and_reset<T, F>(target: &mut T, reset_fn: F) -> T
    where
        T: Default,
        F: FnOnce(&T) -> T,
    {
        let old = std::mem::take(target);
        *target = reset_fn(&old);
        old
    }
    pub fn compute_hash<T>(value: &T) -> u64
    where
        T: Hash + ?Sized,
    {
        let mut hasher = DefaultHasher::new();
        value.hash(&mut hasher);
        hasher.finish()
    }
}

pub trait ToInt {
    fn to_int(&self) -> i32;
}

impl ToInt for Ordering {
    fn to_int(&self) -> i32 {
        match self {
            Ordering::Less => -1,
            Ordering::Equal => 0,
            Ordering::Greater => 1,
        }
    }
}

/// Extension trait for `Option<T>` that provides a convenient way to
/// temporarily take ownership of the inner value, operate on it, and
/// then restore it back into the `Option`.
///
/// This is useful for patterns where you need a mutable borrow of the
/// contents, perform some fallible operation, and then put the value
/// back—without panicking or manually handling `take()` / `replace()`.
pub trait OptionTakeExt<T> {
    /// Takes the inner `T` out of the `Option`, leaving `None` in its place,
    /// then calls the provided closure `f` on a mutable reference to that `T`.
    ///
    /// - If the `Option` was `Some(val)`, runs `f(&mut val)`, restores
    ///   `Some(val)`, and returns the closure’s `Result<R>`.
    /// - If the `Option` was `None`, returns an `Err` with a
    ///   `LuceneError::illegal_state("Option was None".to_string())`.
    ///
    /// # Errors
    ///
    /// Returns `Err(LuceneError::illegal_state)` if the `Option` is empty,
    /// or propagates any `Err` returned by the closure.
    fn take_do_return<R>(&mut self, f: impl FnOnce(&mut T) -> Result<R>) -> Result<R>;
}

impl<T> OptionTakeExt<T> for Option<T> {
    /// Implementation of `take_do_return` for all `Option<T>`.
    ///
    /// 1. Calls `self.take()` to extract the value (or return an error if
    ///    `None`).
    /// 2. Runs the user-provided closure on a mutable reference to the value.
    /// 3. Restores the value back into `self` regardless of success or failure.
    /// 4. Returns the `Result<R>` produced by the closure.
    fn take_do_return<R>(&mut self, f: impl FnOnce(&mut T) -> Result<R>) -> Result<R> {
        let mut val = self
            .take()
            .ok_or_else(|| LuceneError::illegal_state("Option was None"))?;
        let res = f(&mut val);
        *self = Some(val);
        res
    }
}

/// Converts a signed integer to `usize` with overflow checking.
///
/// This trait ensures safe conversion from signed integers (`i16`, `i32`,
/// `i64`) to `usize`, explicitly rejecting negative values to prevent
/// unintended behavior.
///
/// **Important:** In Rust, casting a negative value using `as` (e.g., `-1_i32
/// as usize`) will not panic. Instead, it wraps around and produces a large
/// `usize` value (e.g., `usize::MAX` on most platforms). This trait avoids that
/// risk by returning an error.
///
/// # Examples
///
/// ```
/// use rlucene::core::util::ToUsizeExact;
/// use rlucene::core::util::error::lucene_error::LuceneError;
///
/// let x: i32 = 10;
/// let u = x.to_usize_exact();
/// assert!(u.is_ok());
///
/// let x: i32 = -5;
/// let u = x.to_usize_exact();
/// assert!(matches!(u.unwrap_err(), LuceneError::IllegalState(_)));
/// // Without this trait:
/// let bad: usize = -1_i32 as usize; // 18446744073709551615 on 64-bit platforms not we expected
/// ```
///
/// # Errors
///
/// Returns `LuceneError::number_overflow` if the value is negative.
///
/// # See Also
///
/// - [`TryFrom<i32> for usize`](https://doc.rust-lang.org/std/convert/trait.TryFrom.html)
/// - [`as` casting pitfalls](https://doc.rust-lang.org/reference/expressions/operator-expr.html#type-cast-expressions)
pub trait ToUsizeExact {
    /// Performs a checked conversion to `usize`, returning an error if the
    /// value is negative.
    fn to_usize_exact(self) -> Result<usize>;
}

macro_rules! impl_to_usize_exact {
    ($($t:ty),+) => {
        $(
            impl ToUsizeExact for $t {
                fn to_usize_exact(self) -> Result<usize> {
                    if self < 0 {
                        Err(LuceneError::illegal_state(format!(
                            "negative value cannot be converted to usize: {}", self
                        )))
                    } else {
                        Ok(self as usize)
                    }
                }
            }
        )+
    };
}

impl_to_usize_exact!(i16, i32, i64);

pub trait BitSetExt {
    fn next_set_bit(&self, from: usize) -> i32;
}
impl BitSetExt for BitSet {
    // TODO: this method Need optimization
    fn next_set_bit(&self, from: usize) -> i32 {
        match self.iter().find(|&bit| bit >= from) {
            Some(bit) => bit as i32,
            None => -1,
        }
    }
}
pub trait OutputIdentity {
    fn is_same_reference(&self, other: &Self) -> bool;
}
impl OutputIdentity for Rc<i64> {
    fn is_same_reference(&self, other: &Self) -> bool {
        Rc::ptr_eq(self, other)
    }
}
impl OutputIdentity for BytesRef<Rc<Vec<u8>>> {
    fn is_same_reference(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.bytes, &other.bytes)
            && self.offset == other.offset
            && self.length == other.length
    }
}
impl OutputIdentity for BytesRef<Rc<RefCell<Vec<u8>>>> {
    fn is_same_reference(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.bytes, &other.bytes)
            && self.offset == other.offset
            && self.length == other.length
    }
}
impl OutputIdentity for IntsRef<Rc<Vec<i32>>> {
    fn is_same_reference(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.ints, &other.ints)
            && self.offset == other.offset
            && self.length == other.length
    }
}
impl OutputIdentity for IntsRef<Rc<RefCell<Vec<i32>>>> {
    fn is_same_reference(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.ints, &other.ints)
            && self.offset == other.offset
            && self.length == other.length
    }
}

pub trait HashCode {
    fn hash_code(&self) -> i32;
}
// i32
impl HashCode for i32 {
    fn hash_code(&self) -> i32 {
        *self
    }
}
// i64
impl HashCode for i64 {
    fn hash_code(&self) -> i32 {
        let value = *self as u64;
        let high = value >> 32;
        let mixed = value ^ high;
        (mixed & 0xFFFF_FFFF) as i32
    }
}
// Rc<i64>
impl HashCode for Rc<i64> {
    fn hash_code(&self) -> i32 {
        (**self).hash_code()
    }
}

#[derive(Clone)]
pub struct IdentityRc<T> {
    pub object: Rc<T>,
}
impl<T> IdentityRc<T> {
    pub fn new(object: Rc<T>) -> Self {
        Self { object }
    }
}
impl<T> PartialEq for IdentityRc<T> {
    fn eq(&self, other: &Self) -> bool {
        Rc::as_ptr(&self.object) == Rc::as_ptr(&other.object)
    }
}
impl<T> Eq for IdentityRc<T> {}

impl<T> Hash for IdentityRc<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Rc::as_ptr(&self.object).hash(state)
    }
}
