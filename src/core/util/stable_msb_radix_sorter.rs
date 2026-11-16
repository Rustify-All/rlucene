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

use crate::core::util::error::lucene_error::Result;
use crate::core::util::{
  BINARY_SORT_THRESHOLD, HISTOGRAM_SIZE, MSBRadixSorterBase, Sorter, check_range,
};

pub struct StableMSBRadixSorter<T> {
  delegate: T,
  fixed_start_offsets: [usize; HISTOGRAM_SIZE],
  max_length: usize,
}

impl<T> StableMSBRadixSorter<T> {
  pub fn new(delegate: T, max_length: usize) -> StableMSBRadixSorter<T> {
    StableMSBRadixSorter {
      delegate,
      fixed_start_offsets: [0; HISTOGRAM_SIZE],
      max_length,
    }
  }
}

impl<T> Sorter for StableMSBRadixSorter<T> where T: StableMSBRadixSorterBase {}

impl<T> MSBRadixSorterBase for StableMSBRadixSorter<T>
where
  T: StableMSBRadixSorterBase,
{
  #[inline(always)]
  fn byte_at(&mut self, i: usize, k: usize) -> Result<i32> {
    self.delegate.byte_at(i, k)
  }

  fn get_fallback_sorter(&mut self, k: usize, _length: usize) -> impl Sorter {
    StableMSBRadixSorterBase::get_fallback_sorter(&mut self.delegate, k, self.max_length)
  }

  fn get_bucket(&mut self, i: usize, k: usize) -> Result<usize> {
    self.delegate.get_bucket(i, k)
  }

  fn build_histogram(
    &mut self,
    prefix_common_bucket: usize,
    prefix_common_len: usize,
    from: usize,
    to: usize,
    k: usize,
    histogram: &mut [usize],
  ) -> Result<()> {
    self.delegate.build_histogram(
      prefix_common_bucket,
      prefix_common_len,
      from,
      to,
      k,
      histogram,
    )
  }

  fn should_fallback(&self, from: usize, to: usize, l: usize) -> bool {
    self.delegate.should_fallback(from, to, l)
  }

  fn reorder(
    &mut self,
    from: usize,
    to: usize,
    start_offsets: &mut [usize],
    end_offsets: &mut [usize],
    k: usize,
  ) -> Result<()> {
    // Copy start_offsets to fixed_start_offsets
    self.fixed_start_offsets[..start_offsets.len()].copy_from_slice(start_offsets);

    for (i, &limit) in end_offsets.iter().enumerate().take(HISTOGRAM_SIZE) {
      let mut h1 = self.fixed_start_offsets[i];
      while h1 < limit {
        let b = self.get_bucket(from + h1, k)?;
        let h2 = start_offsets[b];
        start_offsets[b] += 1;
        self.delegate.save(from + h1, from + h2);
        h1 += 1;
      }
    }

    self.delegate.restore(from, to);
    Ok(())
  }
}

pub trait StableMSBRadixSorterBase: MSBRadixSorterBase {
  /// Save the i-th value into the j-th position in temporary storage.
  fn save(&mut self, i: usize, j: usize);
  /// Restore values between i-th and j-th(excluding) in temporary storage
  /// into original storage.
  fn restore(&mut self, i: usize, j: usize);

  fn get_fallback_sorter(&mut self, k: usize, length: usize) -> impl Sorter
  where
    Self: Sized,
  {
    let delegate = MergeSorterImpl::new(k, length, self);
    MergeSorter {
      delegate,
      pivot_index: 0,
    }
  }
}

pub struct MergeSorter<T> {
  pub(crate) delegate: T,
  pub(crate) pivot_index: usize,
}

impl<T> MergeSorter<T>
where
  T: StableMSBRadixSorterBase,
{
  fn merge_sort(&mut self, from: usize, to: usize) -> Result<()> {
    if to - from < BINARY_SORT_THRESHOLD {
      self.binary_sort(from, to)
    } else {
      let mid = (from + to) / 2;
      self.merge_sort(from, mid)?;
      self.merge_sort(mid, to)?;
      self.merge(from, to, mid)
    }
  }
  /// We tried to expose this to implementations to get a bulk copy
  /// optimization. However, it did not bring a noticeable improvement in
  /// benchmarks as `len` is usually small.
  fn bulk_save(&mut self, from: usize, tmp_from: usize, len: usize) {
    for i in 0..len {
      self.delegate.save(from + i, tmp_from + i);
    }
  }
  fn merge(&mut self, from: usize, to: usize, mid: usize) -> Result<()> {
    debug_assert!(
      to > mid && mid > from,
      "Invalid indices: to={to}, mid={mid}, from={from}"
    );
    // If already sorted, return early
    if self.delegate.compare(mid - 1, mid)? <= 0 {
      return Ok(());
    }
    let mut left = from;
    let mut right = mid;
    let mut index = from;
    loop {
      let cmp = self.delegate.compare(left, right)?;

      if cmp <= 0 {
        self.delegate.save(left, index);
        left += 1;
        index += 1;

        if left == mid {
          debug_assert_eq!(index, right, "Index mismatch: index={index}, right={right}");
          self.bulk_save(right, index, to - right);
          break;
        }
      } else {
        self.delegate.save(right, index);
        right += 1;
        index += 1;

        if right == to {
          debug_assert_eq!(
            to - index,
            mid - left,
            "Range mismatch: to-index={}, mid-left={}",
            to - index,
            mid - left
          );
          self.bulk_save(left, index, mid - left);
          break;
        }
      }
    }
    self.delegate.restore(from, to);
    Ok(())
  }
}
impl<T> Sorter for MergeSorter<T>
where
  T: StableMSBRadixSorterBase,
{
  fn compare(&mut self, i: usize, j: usize) -> Result<i32> {
    self.delegate.compare(i, j)
  }

  fn swap(&mut self, i: usize, j: usize) -> Result<()> {
    self.delegate.swap(i, j)
  }

  fn set_pivot(&mut self, i: usize) -> Result<()> {
    self.pivot_index = i;
    Ok(())
  }

  fn compare_pivot(&mut self, i: usize) -> Result<i32> {
    self.compare(self.pivot_index, i)
  }

  fn sort(&mut self, from: usize, to: usize) -> Result<()> {
    check_range(from, to)?;
    self.merge_sort(from, to)?;
    Ok(())
  }
}

pub struct MergeSorterImpl<'a, T> {
  k: usize,
  max_length: usize,
  delegate: &'a mut T,
}
impl<'a, T> MergeSorterImpl<'a, T> {
  pub fn new(k: usize, max_length: usize, delegate: &'a mut T) -> MergeSorterImpl<'a, T> {
    MergeSorterImpl {
      k,
      max_length,
      delegate,
    }
  }
}
impl<T> Sorter for MergeSorterImpl<'_, T>
where
  T: StableMSBRadixSorterBase,
{
  #[inline(always)]
  fn compare(&mut self, i: usize, j: usize) -> Result<i32> {
    for o in self.k..self.max_length {
      let b1 = self.delegate.byte_at(i, o)?;
      let b2 = self.delegate.byte_at(j, o)?;
      if b1 != b2 {
        return Ok(b1 - b2);
      } else if b1 == -1 {
        break;
      }
    }
    Ok(0)
  }
  fn swap(&mut self, i: usize, j: usize) -> Result<()> {
    self.delegate.swap(i, j)
  }
}

impl<T> MSBRadixSorterBase for MergeSorterImpl<'_, T> where
  T: MSBRadixSorterBase + Sorter + StableMSBRadixSorterBase
{
}

impl<T> StableMSBRadixSorterBase for MergeSorterImpl<'_, T>
where
  T: StableMSBRadixSorterBase,
{
  fn save(&mut self, i: usize, j: usize) {
    self.delegate.save(i, j);
  }

  fn restore(&mut self, i: usize, j: usize) {
    self.delegate.restore(i, j);
  }
}
