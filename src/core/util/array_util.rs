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

use crate::core::util::array_tim_sorter::ArrayTimSorter;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::bkd::BKDUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::selector::Selector;
use crate::core::util::{
  ArrayIntroSorter, Comparator, IntroSelector, IntroSelectorBase, IntroSelectorBaseDefault,
  NaturalOrder, Sorter, ToInt,
};

pub struct ArrayUtil;
impl ArrayUtil {
  /// Maximum number of elements supported by Lucene's array-oriented APIs.
  ///
  /// The element-count limit is `i32::MAX`; [`oversize`](Self::oversize) separately
  /// enforces Rust's allocation-size limit.
  pub const MAX_ARRAY_LENGTH: usize = i32::MAX as usize;
  const MIN_RADIX: i32 = 2;
  const MAX_RADIX: i32 = 36;
  /// Parses a char array into an i32 with the default radix of 10.
  ///
  /// # Arguments
  ///
  /// * `chars` - The character array to parse.
  /// * `offset` - The starting offset in the array.
  /// * `len` - The length of the array to parse.
  ///
  /// # Returns
  ///
  /// * `i32` - The parsed integer.
  ///
  /// # Errors
  ///
  /// Returns a [`LuceneErrorKind::NumberFormat`](crate::core::util::error::lucene_error::LuceneErrorKind::NumberFormat) if it can't parse the chars into
  /// an integer.
  pub fn parse_int_default(chars: &[char], offset: usize, len: usize) -> Result<i32> {
    Self::parse_int(chars, offset, len, 10)
  }

  /// Parses the string argument as if it were an `i32` value and returns the
  /// result. Returns an [`LuceneErrorKind::NumberFormat`](crate::core::util::error::lucene_error::LuceneErrorKind::NumberFormat) if the string does not
  /// represent an `i32` quantity. The second argument specifies the radix
  /// to use when parsing the value.
  pub fn parse_int(chars: &[char], mut offset: usize, mut len: usize, radix: i32) -> Result<i32> {
    if !(ArrayUtil::MIN_RADIX..=ArrayUtil::MAX_RADIX).contains(&radix) {
      return Err(LuceneError::number_format("Invalid radix"));
    }
    if len == 0 {
      return Err(LuceneError::number_format("chars length is 0"));
    }
    let mut i = 0;
    let negative = chars[offset + i] == '-';
    if negative {
      i += 1;
      if i == len {
        return Err(LuceneError::number_format("chars length is 0"));
      }
    }
    if negative {
      offset += 1;
      len -= 1;
    }
    Self::parse(chars, offset, len, radix, negative)
  }

  pub fn parse(
    chars: &[char],
    offset: usize,
    len: usize,
    radix: i32,
    negative: bool,
  ) -> Result<i32> {
    let max = i32::MIN / radix;
    let mut result = 0;
    for i in 0..len {
      let digit = chars[offset + i]
        .to_digit(radix as u32)
        .ok_or_else(|| LuceneError::number_format("Unable to parse"))?;
      if max > result {
        return Err(LuceneError::number_format("Unable to parse"));
      }

      let next = result
        .checked_mul(radix)
        .and_then(|x| x.checked_sub(digit as i32));

      match next {
        Some(next) => {
          if next > result {
            return Err(LuceneError::number_format("Unable to parse"));
          }
          result = next;
        },
        None => return Err(LuceneError::number_format("Unable to parse")),
      }
    }
    if !negative {
      result = -result;
      if result < 0 {
        return Err(LuceneError::number_format("Unable to parse"));
      }
    }
    Ok(result)
  }
  /// Returns a vector length greater than or equal to `min_target_size`,
  /// generally over-allocating exponentially to achieve amortized linear-time
  /// cost as the vector grows.
  ///
  /// Grows by one eighth, with a minimum growth of three elements for small
  /// vectors. The allocator handles the alignment required by the element type.
  ///
  /// `bytes_per_element` is used to ensure that the vector's element storage
  /// does not exceed Rust's `isize::MAX` byte allocation limit. A value of zero
  /// is accepted for compatibility with existing Lucene callers that do not
  /// need an element-size-specific limit.
  ///
  /// # Errors
  ///
  /// Returns [`LuceneErrorKind::IllegalArgument`](crate::core::util::error::lucene_error::LuceneErrorKind::IllegalArgument) when `min_target_size` exceeds
  /// either Lucene's maximum supported vector length or Rust's maximum
  /// allocation size for the given element size.
  #[inline]
  pub fn oversize(min_target_size: usize, bytes_per_element: usize) -> Result<usize> {
    if min_target_size == 0 {
      // Wait until at least one element is requested.
      return Ok(0);
    }

    let max_array_length = (isize::MAX as usize)
      .checked_div(bytes_per_element)
      .map(|v| Self::MAX_ARRAY_LENGTH.min(v))
      .unwrap_or(Self::MAX_ARRAY_LENGTH);

    if min_target_size > max_array_length {
      return Err(LuceneError::illegal_argument(format!(
        "requested vector size {min_target_size} exceeds maximum vector length \
         {max_array_length} for elements of {bytes_per_element} bytes"
      )));
    }

    // Asymptotic exponential growth by one eighth favors spending a bit more
    // CPU in order to avoid tying up too much unused RAM.
    let mut extra = min_target_size >> 3;

    if extra < 3 {
      // For very small vectors, where the constant overhead of reallocation is
      // presumably relatively high, grow faster.
      extra = 3;
    }

    Ok(
      min_target_size
        .checked_add(extra)
        .unwrap_or(max_array_length)
        .min(max_array_length),
    )
  }
  /// Grows to exactly `new_length`, constructing added elements with `init`.
  pub fn grow_exact<T, F>(vec: &mut Vec<T>, new_length: usize, init: F) -> Result<()>
  where
    F: FnMut() -> T,
  {
    let current_length = vec.len();
    match new_length.cmp(&current_length) {
      Ordering::Greater => {
        vec.reserve_exact(new_length - current_length);
        vec.resize_with(new_length, init);
      },
      Ordering::Equal => {
        return Ok(());
      },
      Ordering::Less => {
        return Err(LuceneError::array_index_out_of_bounds(format!(
          "new_length: {new_length} is less than current_length: {current_length}"
        )));
      },
    }
    Ok(())
  }
  /// Reserves space for at least `min_size` elements without changing the length.
  ///
  /// Checks Lucene's element-count and Rust's allocation-size limits, then lets
  /// `Vec` choose its capacity growth. No elements are constructed.
  pub fn grow_capacity<T>(vec: &mut Vec<T>, min_size: usize) -> Result<()> {
    // Only use oversize for its existing size checks, not as a second allocation
    // growth policy on top of Vec::reserve.
    Self::oversize(min_size, size_of::<T>())?;
    vec.reserve(min_size.saturating_sub(vec.len()));
    Ok(())
  }
  /// Ensures at least `min_size` initialized elements, preserving existing contents.
  /// Only the missing elements up to `min_size` are constructed with `init`;
  /// any spare capacity remains uninitialized.
  /// The initializer is not called when the existing length is sufficient.
  pub fn grow_with<T, F>(vec: &mut Vec<T>, min_size: usize, init: F) -> Result<()>
  where
    F: FnMut() -> T,
  {
    if vec.len() < min_size {
      Self::grow_capacity(vec, min_size)?;
      vec.resize_with(min_size, init);
    }
    Ok(())
  }
  /// Ensures at least `min_size` initialized elements for scratch use.
  /// Existing contents may be discarded when a new allocation is needed.
  /// Only `min_size` elements are initialized; spare capacity is not filled.
  pub fn grow_no_copy<T, F>(vec: &mut Vec<T>, min_size: usize, init: F) -> Result<()>
  where
    F: FnMut() -> T,
  {
    let current_size = vec.len();
    if current_size < min_size {
      let new_capacity = Self::oversize(min_size, size_of::<T>())?;
      if vec.capacity() >= min_size {
        vec.resize_with(min_size, init);
      } else {
        let mut replacement = Vec::with_capacity(new_capacity);
        replacement.resize_with(min_size, init);
        *vec = replacement;
      }
    }
    Ok(())
  }
  /// Returns the hash of chars in the range from `start` (inclusive) to `end`
  /// (inclusive).
  pub fn hash_code(array: &[char], start: usize, end: usize) -> i32 {
    let mut code: i32 = 0;
    for i in (start..end).rev() {
      code = code.wrapping_mul(31).wrapping_add(array[i] as i32);
    }
    code
  }

  /// Sorts the given slice using the intro sort algorithm,
  /// falling back to insertion sort for small arrays.
  pub fn do_intro_sort<T, C>(a: &mut [T], from_index: usize, to_index: usize, comp: C) -> Result<()>
  where
    C: Comparator<T>,
  {
    if to_index.saturating_sub(from_index) <= 1 {
      return Ok(());
    }
    ArrayIntroSorter::new(a, comp).sort(from_index, to_index)
  }
  /// Sorts the given slice using the intro sort algorithm,
  /// falling back to insertion sort for small arrays.
  pub fn intro_sort_with_comparator<T, C>(a: &mut [T], comp: C) -> Result<()>
  where
    C: Comparator<T>,
  {
    Self::do_intro_sort(a, 0, a.len(), comp)
  }
  /// Sorts the given slice in natural order using the intro sort algorithm,
  /// falling back to insertion sort for small arrays.
  pub fn intro_sort_with_range<T>(a: &mut [T], from_index: usize, to_index: usize) -> Result<()>
  where
    T: Ord,
  {
    if to_index <= 1 + from_index {
      return Ok(());
    }
    Self::do_intro_sort(a, from_index, to_index, NaturalOrder::new())
  }
  /// Sorts the given slice in natural order using the intro sort algorithm,
  /// falling back to insertion sort for small arrays.
  pub fn intro_sort<T>(a: &mut [T]) -> Result<()>
  where
    T: Ord,
  {
    Self::intro_sort_with_range(a, 0, a.len())
  }
  /// Sorts the given slice using the Tim sort algorithm
  /// falling back to binary sort for small arrays.
  pub fn do_tim_sort<T, C>(a: &mut [T], from_index: usize, to_index: usize, comp: C) -> Result<()>
  where
    T: Copy,
    C: Comparator<T>,
  {
    if to_index <= 1 + from_index {
      return Ok(());
    }
    let max_temp_slots = a.len() / 64;
    debug_assert!(max_temp_slots <= i32::MAX as usize);
    ArrayTimSorter::new(a, comp, max_temp_slots).sort(from_index, to_index)
  }
  /// Sorts the given slice using the Tim sort algorithm,
  /// falling back to binary sort for small arrays.
  pub fn tim_sort_with_comparator<T, C>(a: &mut [T], comp: C) -> Result<()>
  where
    T: Copy,
    C: Comparator<T>,
  {
    let len = a.len();
    Self::do_tim_sort(a, 0, len, comp)
  }
  /// Sorts the given slice in natural order using the Tim sort algorithm,
  /// falling back to binary sort for small arrays.
  pub fn tim_sort_with_range<T>(a: &mut [T], from_index: usize, to_index: usize) -> Result<()>
  where
    T: Copy + Ord,
  {
    if to_index <= 1 + from_index {
      return Ok(());
    }
    Self::do_tim_sort(a, from_index, to_index, NaturalOrder::new())
  }
  /// Sorts the given slice in natural order using the Tim sort algorithm,
  /// falling back to binary sort for small arrays.
  pub fn tim_sort<T>(a: &mut [T]) -> Result<()>
  where
    T: Copy + Ord,
  {
    let len = a.len();
    Self::tim_sort_with_range(a, 0, len)
  }

  /// Reorganize the slice `arr[from..to]` so that the element at offset `k`
  /// is at the same position as if `arr[from..to]` were sorted, and all
  /// elements to its left are less than or equal to it, and all elements
  /// to its right are greater than or equal to it.
  ///
  /// This runs in linear time on average and in `n*log(n)` time in the worst
  /// case.
  ///
  /// # Parameters
  /// - `arr`: The array to be re-organized.
  /// - `from`: The starting index for re-organization. Elements before this
  ///   index will be left as is.
  /// - `to`: The ending index. Elements after this index will be left as is.
  /// - `k`: The index of the element to sort from. Value must be less than
  ///   `to` and greater than or equal to `from`.
  /// - `comparator`: A comparator to use for sorting.
  pub fn select<T, C>(arr: &mut [T], from: usize, to: usize, k: usize, comparator: &C) -> Result<()>
  where
    C: Comparator<T>,
  {
    let sub_selector = IntroSelectorImpl::new(arr, comparator);
    let mut selector = IntroSelector::new(sub_selector);
    Selector::select(&mut selector, from, to, k)?;
    Ok(())
  }
  /// Copies a slice into a new vector.
  pub fn copy_array<T>(array: &[T]) -> Vec<T>
  where
    T: Clone,
  {
    Self::copy_of_sub_array(array, 0, array.len())
  }
  pub fn copy_of_sub_array<T>(array: &[T], from: usize, to: usize) -> Vec<T>
  where
    T: Clone,
  {
    debug_assert!(to >= from && to <= array.len());
    array[from..to].to_vec()
  }
  /// Returns a comparator for exactly the specified number of bytes.
  pub fn get_unsigned_comparator(num_bytes: usize) -> ByteArrayComparatorEnum {
    if num_bytes == BitUtil::LONG_BYTES {
      // Used by LongPoint, DoublePoint
      return ByteArrayComparatorEnum::U64(U64byteArrayComparator);
    } else if num_bytes == BitUtil::INT_BYTES {
      // Used by IntPoint, FloatPoint, LatLonPoint, LatLonShape
      return ByteArrayComparatorEnum::U32(U32byteArrayComparator);
    }
    ByteArrayComparatorEnum::Byte(ByteByteArrayComparator { num_bytes })
  }
}

struct IntroSelectorImpl<'a, T, C> {
  pivot: usize,
  arr: &'a mut [T],
  comparator: &'a C,
}
impl<'a, T, C> IntroSelectorImpl<'a, T, C> {
  fn new(arr: &'a mut [T], comparator: &'a C) -> IntroSelectorImpl<'a, T, C> {
    IntroSelectorImpl {
      pivot: 0,
      arr,
      comparator,
    }
  }
}

impl<T, C> IntroSelectorBaseDefault for IntroSelectorImpl<'_, T, C>
where
  C: Comparator<T>,
{
  fn set_pivot(&mut self, i: usize) -> Result<()> {
    self.pivot = i;
    Ok(())
  }

  fn compare_pivot(&mut self, j: usize) -> Result<i32> {
    self.comparator.compare(&self.arr[self.pivot], &self.arr[j])
  }
}

impl<T, C> IntroSelectorBase for IntroSelectorImpl<'_, T, C> where C: Comparator<T> {}
impl<T, C> Selector for IntroSelectorImpl<'_, T, C>
where
  C: Comparator<T>,
{
  fn swap(&mut self, i: usize, j: usize) -> Result<()> {
    // The data pointed to by the pivot has been swapped.
    // We need to adjust the pivot value to ensure that
    // the value corresponding to the pivot remains unchanged.
    // To avoid Copying the value, we just swap the pivot index.
    if self.pivot == i || self.pivot == j {
      self.pivot = if self.pivot == i { j } else { i };
    }
    self.arr.swap(i, j);
    Ok(())
  }
}
/// Comparator for a fixed number of bytes.
pub trait ByteArrayComparator {
  /// Compare bytes starting from the given offsets.
  ///
  /// The return value has the same contract as [`Ord::cmp`].
  fn compare(&self, a: &[u8], a_i: usize, b: &[u8], b_i: usize) -> i32;
}
#[derive(Clone)]
pub enum ByteArrayComparatorEnum {
  U64(U64byteArrayComparator),
  U32(U32byteArrayComparator),
  Byte(ByteByteArrayComparator),
  CommonPrefixLength8(CommonPrefixLength8),
  CommonPrefixLength4(CommonPrefixLength4),
  CommonPrefixLength(CommonPrefixLengthN),
}
impl ByteArrayComparator for ByteArrayComparatorEnum {
  fn compare(&self, a: &[u8], a_i: usize, b: &[u8], b_i: usize) -> i32 {
    match self {
      ByteArrayComparatorEnum::U64(c) => c.compare(a, a_i, b, b_i),
      ByteArrayComparatorEnum::U32(c) => c.compare(a, a_i, b, b_i),
      ByteArrayComparatorEnum::Byte(c) => c.compare(a, a_i, b, b_i),
      ByteArrayComparatorEnum::CommonPrefixLength8(c) => c.compare(a, a_i, b, b_i),
      ByteArrayComparatorEnum::CommonPrefixLength4(c) => c.compare(a, a_i, b, b_i),
      ByteArrayComparatorEnum::CommonPrefixLength(c) => c.compare(a, a_i, b, b_i),
    }
  }
}

#[derive(Clone)]
pub struct U64byteArrayComparator;
impl ByteArrayComparator for U64byteArrayComparator {
  fn compare(&self, a: &[u8], a_i: usize, b: &[u8], b_i: usize) -> i32 {
    (BitUtil::get_i64_be(a, a_i) as u64)
      .cmp(&(BitUtil::get_i64_be(b, b_i) as u64))
      .to_int()
  }
}
#[derive(Clone)]
pub struct U32byteArrayComparator;
impl ByteArrayComparator for U32byteArrayComparator {
  fn compare(&self, a: &[u8], a_i: usize, b: &[u8], b_i: usize) -> i32 {
    (BitUtil::get_i32_be(a, a_i) as u32)
      .cmp(&(BitUtil::get_i32_be(b, b_i) as u32))
      .to_int()
  }
}
#[derive(Clone)]
pub struct ByteByteArrayComparator {
  num_bytes: usize,
}
impl ByteArrayComparator for ByteByteArrayComparator {
  fn compare(&self, a: &[u8], a_i: usize, b: &[u8], b_i: usize) -> i32 {
    debug_assert!(a.len() >= a_i + self.num_bytes);
    debug_assert!(b.len() >= b_i + self.num_bytes);
    a[a_i..a_i + self.num_bytes]
      .cmp(&b[b_i..b_i + self.num_bytes])
      .to_int()
  }
}
#[derive(Clone)]
pub struct CommonPrefixLength8;
impl ByteArrayComparator for CommonPrefixLength8 {
  fn compare(&self, a: &[u8], a_i: usize, b: &[u8], b_i: usize) -> i32 {
    BKDUtil::common_prefix_length8(a, a_i, b, b_i)
  }
}
#[derive(Clone)]
pub struct CommonPrefixLength4;
impl ByteArrayComparator for CommonPrefixLength4 {
  fn compare(&self, a: &[u8], a_i: usize, b: &[u8], b_i: usize) -> i32 {
    BKDUtil::common_prefix_length4(a, a_i, b, b_i)
  }
}
#[derive(Clone)]
pub struct CommonPrefixLengthN {
  pub(crate) num_bytes: usize,
}
impl ByteArrayComparator for CommonPrefixLengthN {
  fn compare(&self, a: &[u8], a_i: usize, b: &[u8], b_i: usize) -> i32 {
    BKDUtil::common_prefix_length_n(a, a_i, b, b_i, self.num_bytes)
  }
}
