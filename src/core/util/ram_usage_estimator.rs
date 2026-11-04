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
use std::collections::{HashMap, VecDeque};
use std::mem;

use crate::core::util::accountable::Accountable;
use crate::core::util::error::lucene_error::Result;
#[allow(dead_code)] // for quick search
struct RamUsageEstimator;

pub const QUERY_DEFAULT_RAM_BYTES_USED: i64 = 1024;

fn usize_to_i64_saturating(value: usize) -> i64 {
  if value > i64::MAX as usize {
    i64::MAX
  } else {
    value as i64
  }
}

/// Primitive value types whose sizes are commonly needed by memory accounting.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PrimitiveType {
  Boolean,
  Byte,
  Char,
  Short,
  Int,
  Float,
  Double,
  Long,
}

/// One kilobyte bytes.
pub const ONE_KB: i64 = 1024;

/// One megabyte bytes.
pub const ONE_MB: i64 = ONE_KB * ONE_KB;

/// One gigabyte bytes.
pub const ONE_GB: i64 = ONE_KB * ONE_MB;

/// Sizes of primitive types.
pub const PRIMITIVE_SIZES: [(PrimitiveType, i64); 8] = [
  (PrimitiveType::Boolean, mem::size_of::<bool>() as i64),
  (PrimitiveType::Byte, mem::size_of::<u8>() as i64),
  (PrimitiveType::Char, mem::size_of::<char>() as i64),
  (PrimitiveType::Short, mem::size_of::<i16>() as i64),
  (PrimitiveType::Int, mem::size_of::<i32>() as i64),
  (PrimitiveType::Float, mem::size_of::<f32>() as i64),
  (PrimitiveType::Double, mem::size_of::<f64>() as i64),
  (PrimitiveType::Long, mem::size_of::<i64>() as i64),
];

/// Return the size of the provided primitive type.
pub const fn primitive_size(primitive_type: PrimitiveType) -> i64 {
  match primitive_type {
    PrimitiveType::Boolean => mem::size_of::<bool>() as i64,
    PrimitiveType::Byte => mem::size_of::<u8>() as i64,
    PrimitiveType::Char => mem::size_of::<char>() as i64,
    PrimitiveType::Short => mem::size_of::<i16>() as i64,
    PrimitiveType::Int => mem::size_of::<i32>() as i64,
    PrimitiveType::Float => mem::size_of::<f32>() as i64,
    PrimitiveType::Double => mem::size_of::<f64>() as i64,
    PrimitiveType::Long => mem::size_of::<i64>() as i64,
  }
}

/// Returns the retained heap size in bytes of an owned `Vec` buffer.
///
/// This counts the allocation capacity rather than the current length,
/// matching the amount of memory retained by the `Vec`. This does not count
/// the inline `Vec` control value itself, since it may live on the stack or
/// inside another allocation.
#[allow(clippy::ptr_arg)]
pub fn size_of_vec<T>(vec: &Vec<T>) -> i64 {
  size_of_array_capacity::<T>(vec.capacity())
}

/// Returns the retained heap size in bytes of an owned `VecDeque` buffer.
///
/// This counts the allocation capacity rather than the current length and
/// does not recursively count heap memory owned by initialized elements.
pub fn size_of_vec_deque<T>(deque: &VecDeque<T>) -> i64 {
  size_of_array_capacity::<T>(deque.capacity())
}

/// Returns a lower-bound estimate of the retained heap size in bytes of a
/// `HashMap` bucket allocation.
///
/// This counts the public capacity reserved for inline keys and values. It does
/// not count private control bytes, allocator metadata, or recursively count
/// heap memory owned by initialized keys or values.
pub fn size_of_hash_map<K, V>(map: &HashMap<K, V>) -> i64 {
  size_of_array_capacity::<(K, V)>(map.capacity())
}

/// Returns the retained heap size in bytes of a `Vec<String>`.
///
/// This includes the `Vec` buffer that stores inline `String` control values
/// and the heap buffer owned by every initialized `String`. It does not count
/// the inline `Vec` control value itself.
#[allow(clippy::ptr_arg)]
pub fn size_of_string_vec(vec: &Vec<String>) -> i64 {
  let mut size = size_of_vec(vec);
  for s in vec {
    size = size.saturating_add(size_of_string(s));
  }
  size
}

pub fn size_of_slice<T>(arr: &[T]) -> i64 {
  size_of_array_capacity::<T>(arr.len())
}

/// Returns the size in bytes of the [`Accountable`] object, using its
/// [`Accountable::ram_bytes_used`] method.
pub fn size_of_accountable<A>(accountable: &A) -> Result<i64>
where
  A: Accountable,
{
  accountable.ram_bytes_used()
}

/// Returns the retained heap size in bytes of the `String` buffer.
///
/// This does not count the inline `String` control value itself, since it may
/// live on the stack or inside another allocation.
pub fn size_of_string(s: &String) -> i64 {
  size_of_str_capacity(s.capacity())
}

/// Returns `size` in human-readable units (GB, MB, KB or bytes).
pub fn human_readable_units(bytes: i64) -> String {
  if bytes / ONE_GB > 0 {
    let mut formatted = format_with_one_decimal(bytes as f64 / ONE_GB as f64);
    formatted.push_str(" GB");
    formatted
  } else if bytes / ONE_MB > 0 {
    let mut formatted = format_with_one_decimal(bytes as f64 / ONE_MB as f64);
    formatted.push_str(" MB");
    formatted
  } else if bytes / ONE_KB > 0 {
    let mut formatted = format_with_one_decimal(bytes as f64 / ONE_KB as f64);
    formatted.push_str(" KB");
    formatted
  } else {
    format!("{} bytes", bytes)
  }
}

pub(crate) fn size_of_array_capacity<T>(capacity: usize) -> i64 {
  let elem_size = mem::size_of::<T>();
  if elem_size == 0 || capacity == 0 {
    return 0;
  }
  let bytes = capacity.saturating_mul(elem_size);
  usize_to_i64_saturating(bytes)
}

fn size_of_str_capacity(capacity: usize) -> i64 {
  if capacity == 0 {
    return 0;
  }
  usize_to_i64_saturating(capacity)
}

fn format_with_one_decimal(value: f64) -> String {
  let mut formatted = format!("{:.1}", value);
  if formatted.ends_with(".0") {
    formatted.truncate(formatted.len() - 2);
  }
  formatted
}

impl Accountable for () {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(0)
  }
}

impl Accountable for bool {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(0)
  }
}

impl Accountable for char {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(0)
  }
}

impl Accountable for i8 {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(0)
  }
}

impl Accountable for i16 {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(0)
  }
}

impl Accountable for i32 {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(0)
  }
}

impl Accountable for i64 {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(0)
  }
}

impl Accountable for i128 {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(0)
  }
}

impl Accountable for isize {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(0)
  }
}

impl Accountable for u8 {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(0)
  }
}

impl Accountable for u16 {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(0)
  }
}

impl Accountable for u32 {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(0)
  }
}

impl Accountable for u64 {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(0)
  }
}

impl Accountable for u128 {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(0)
  }
}

impl Accountable for usize {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(0)
  }
}

impl Accountable for f32 {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(0)
  }
}

impl Accountable for f64 {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(0)
  }
}

impl Accountable for String {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(size_of_string(self))
  }
}

impl<K, V> Accountable for HashMap<K, V>
where
  K: Accountable,
  V: Accountable,
{
  fn ram_bytes_used(&self) -> Result<i64> {
    let mut size = size_of_hash_map(self);
    for (key, value) in self {
      size = size.saturating_add(key.ram_bytes_used()?);
      size = size.saturating_add(value.ram_bytes_used()?);
    }
    Ok(size)
  }
}
pub(crate) fn size_of_str(s: &str) -> i64 {
  size_of_str_capacity(s.len())
}
