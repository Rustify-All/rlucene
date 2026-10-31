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
use crate::core::index::BytesRefBuilder;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::selector::Selector;
use crate::core::util::{IntroSelector, IntroSelectorBase, IntroSelectorBaseDefault, TryIntoInt};

// Size of histograms: 256 + 1 to indicate that the string is finished.
const HISTOGRAM_SIZE: usize = 257;

pub struct RadixSelector<T> {
  max_length: usize,
  common_prefix: Vec<i32>,
  histogram: [usize; HISTOGRAM_SIZE],
  pub(crate) sub_selector: T,
}

impl<T> RadixSelector<T>
where
  T: RadixSelectorBase,
{
  // after that many levels of recursion we fall back to introselect anyway
  // this is used as a protection against the fact that radix sort performs
  // worse when there are long common prefixes (probably because of cache
  // locality)
  const LEVEL_THRESHOLD: usize = 8;
  // buckets below this size will be sorted with introselect
  const LENGTH_THRESHOLD: usize = 100;

  pub fn new(max_length: usize, sub_selector: T) -> Self {
    RadixSelector {
      max_length,
      common_prefix: vec![0; std::cmp::min(24, max_length)],
      histogram: [0; HISTOGRAM_SIZE],
      sub_selector,
    }
  }

  fn select(&mut self, from: usize, to: usize, k: usize, d: usize, l: usize) -> Result<()> {
    if to - from <= Self::LENGTH_THRESHOLD || l >= Self::LEVEL_THRESHOLD {
      self
        .sub_selector
        .get_fallback_selector(d, self.max_length)
        .select(from, to, k)?;
    } else {
      self.radix_select(from, to, k, d, l)?;
    }
    Ok(())
  }

  /// `d` the character number to compare
  ///
  /// `l` the level of recursion
  pub fn radix_select(
    &mut self,
    from: usize,
    to: usize,
    k: usize,
    d: usize,
    l: usize,
  ) -> Result<()> {
    self.histogram.fill(0);

    let common_prefix_length =
      self.compute_common_prefix_length_and_build_histogram(from, to, d)?;
    if common_prefix_length > 0 {
      // if there are no more chars to compare or if all entries fell into
      // the first bucket (which means strings are shorter
      // than d) then we are done otherwise recurse
      if d + common_prefix_length < self.max_length && self.histogram[0] < to - from {
        self.radix_select(from, to, k, d + common_prefix_length, l)?;
      }
      return Ok(());
    }
    debug_assert!(self.assert_histogram(common_prefix_length, &self.histogram));

    let mut bucket_from = from;
    for bucket in 0..HISTOGRAM_SIZE {
      let bucket_to = bucket_from + self.histogram[bucket];
      if bucket_to > k {
        self.partition(from, to, bucket, bucket_from, bucket_to, d)?;
        if bucket != 0 && d + 1 < self.max_length {
          // all elements in bucket 0 are equal so we only need to
          // recurse if bucket != 0
          self.select(bucket_from, bucket_to, k, d + 1, l + 1)?;
        }
        return Ok(());
      }
      bucket_from = bucket_to;
    }
    Err(LuceneError::unreachable("Unreachable code"))
  }

  // only used from assert
  fn assert_histogram(&self, common_prefix_length: usize, histogram: &[usize]) -> bool {
    let mut number_of_unique_bytes = 0;
    for &freq in histogram.iter() {
      if freq > 0 {
        number_of_unique_bytes += 1;
      }
    }
    if number_of_unique_bytes == 1 {
      debug_assert!(common_prefix_length >= 1);
    } else {
      debug_assert!(common_prefix_length == 0);
    }
    true
  }

  /** Returns a number for the k-th character between 0 and
   * `HISTOGRAM_SIZE`. */
  #[inline]
  fn get_bucket(&mut self, i: usize, k: usize) -> Result<usize> {
    let v = self.sub_selector.byte_at(i, k)? + 1;
    v.try_convert()
  }

  /// Build a histogram of the number of values per [`Self::get_bucket`] and
  /// return a common prefix length for all visited values.
  fn compute_common_prefix_length_and_build_histogram(
    &mut self,
    from: usize,
    to: usize,
    k: usize,
  ) -> Result<usize> {
    let common_prefix_length = self.compute_initial_common_prefix_length(from, k)?;
    self.compute_common_prefix_length_and_build_histogram_part1(from, to, k, common_prefix_length)
  }

  fn compute_initial_common_prefix_length(&mut self, from: usize, k: usize) -> Result<usize> {
    let common_prefix = &mut self.common_prefix;
    let mut common_prefix_length = std::cmp::min(common_prefix.len(), self.max_length - k);
    #[allow(clippy::needless_range_loop)]
    for j in 0..common_prefix_length {
      let b = self.sub_selector.byte_at(from, k + j)?;
      common_prefix[j] = b;
      if b == -1 {
        common_prefix_length = j + 1;
        break;
      }
    }
    Ok(common_prefix_length)
  }

  #[allow(clippy::mut_range_bound)]
  fn compute_common_prefix_length_and_build_histogram_part1(
    &mut self,
    from: usize,
    to: usize,
    k: usize,
    mut common_prefix_length: usize,
  ) -> Result<usize> {
    let common_prefix = &mut self.common_prefix[..common_prefix_length];
    let mut i = from + 1;
    'outer: for current in (from + 1)..=to {
      i = current;
      if i == to {
        break;
      }
      for j in 0..common_prefix_length {
        let b = self.sub_selector.byte_at(current, k + j)?;
        if b != common_prefix[j] {
          common_prefix_length = j;
          if common_prefix_length == 0 {
            let idx: usize = (common_prefix[0] + 1).try_convert()?;
            self.histogram[idx] = i - from;
            let idx1: usize = (b + 1).try_convert()?;
            self.histogram[idx1] = 1;
            break 'outer;
          }
          break;
        }
      }
    }
    self.compute_common_prefix_length_and_build_histogram_part2(
      from,
      to,
      k,
      common_prefix_length,
      i,
    )
  }

  fn compute_common_prefix_length_and_build_histogram_part2(
    &mut self,
    from: usize,
    to: usize,
    k: usize,
    common_prefix_length: usize,
    i: usize,
  ) -> Result<usize> {
    if i < to {
      // the loop got broken because there is no common prefix
      debug_assert!(common_prefix_length == 0);
      self.build_histogram(i + 1, to, k)?;
    } else {
      debug_assert!(common_prefix_length > 0);
      let v: usize = (self.common_prefix[0] + 1).try_convert()?;
      self.histogram[v] = to - from;
    }
    Ok(common_prefix_length)
  }

  /// Build an histogram of the k-th characters of values occurring between
  /// offsets `from` and `to`, using `get_bucket`.
  fn build_histogram(&mut self, from: usize, to: usize, k: usize) -> Result<()> {
    for i in from..to {
      let index = self.get_bucket(i, k)?;
      self.histogram[index] += 1;
    }
    Ok(())
  }

  /// Reorder elements so that all of them that fall into `bucket` are
  /// between offsets `bucketFrom` and `bucketTo`.
  fn partition(
    &mut self,
    from: usize,
    to: usize,
    bucket: usize,
    bucket_from: usize,
    bucket_to: usize,
    d: usize,
  ) -> Result<()> {
    let mut left = from;
    let mut right = to - 1;
    let mut slot = bucket_from;
    loop {
      let mut left_bucket = self.get_bucket(left, d)?;
      let mut right_bucket = self.get_bucket(right, d)?;
      while left_bucket <= bucket && left < bucket_from {
        if left_bucket == bucket {
          self.swap(left, slot)?;
          slot += 1;
        } else {
          left += 1;
        }
        left_bucket = self.get_bucket(left, d)?;
      }
      while right_bucket >= bucket && right >= bucket_to {
        if right_bucket == bucket {
          self.swap(right, slot)?;
          slot += 1;
        } else {
          right -= 1;
        }
        right_bucket = self.get_bucket(right, d)?;
      }
      if left < bucket_from && right >= bucket_to {
        self.swap(left, right)?;
        left += 1;
        right -= 1;
      } else {
        debug_assert!(left == bucket_from);
        debug_assert!(right == bucket_to - 1);
        break;
      }
    }
    Ok(())
  }
  pub fn get_sub_selector(&self) -> &T {
    &self.sub_selector
  }
}

impl<T> Selector for RadixSelector<T>
where
  T: RadixSelectorBase,
{
  fn select(&mut self, from: usize, to: usize, k: usize) -> Result<()> {
    self.check_args(from, to, k)?;
    self.select(from, to, k, 0, 0)
  }

  fn swap(&mut self, i: usize, j: usize) -> Result<()> {
    self.sub_selector.swap(i, j)
  }
}

pub trait RadixSelectorBase: Selector {
  /// Return the k-th byte of the entry at index `i`, or `-1\ if its length is
  /// less than or equal to `k`. This may only be called with a value of
  /// `k` between `0` included and `maxLength` excluded.
  fn byte_at(&mut self, i: usize, k: usize) -> Result<i32>;
  /// Get a fall-back selector which may assume that the first `d` bytes of
  /// all compared strings are equal. This fallback selector is used when
  /// the range becomes narrow or when the maximum level of recursion has
  /// been exceeded.
  fn get_fallback_selector(&mut self, d: usize, max_length: usize) -> impl Selector
  where
    Self: Sized,
  {
    let delegate = IntroSelectorImpl {
      d,
      max_length,
      pivot: BytesRefBuilder::new(),
      delegate: self,
    };
    IntroSelector::new(delegate)
  }
}

pub struct IntroSelectorImpl<'a, T> {
  d: usize,
  max_length: usize,
  pivot: BytesRefBuilder<Vec<u8>>,
  delegate: &'a mut T,
}
impl<T> IntroSelectorBaseDefault for IntroSelectorImpl<'_, T>
where
  T: RadixSelectorBase,
{
  fn set_pivot(&mut self, i: usize) -> Result<()> {
    self.pivot.set_length(0);
    for o in self.d..self.max_length {
      let b = self.delegate.byte_at(i, o)?;
      if b == -1 {
        break;
      }
      self.pivot.append_byte(b as u8)?;
    }
    Ok(())
  }

  fn compare_pivot(&mut self, j: usize) -> Result<i32> {
    for o in 0..self.pivot.length() {
      let b1 = self.pivot.byte_at(o) as i32;
      let b2 = self.delegate.byte_at(j, self.d + o)?;
      if b1 != b2 {
        return Ok(b1 - b2);
      }
    }
    if self.d + self.pivot.length() == self.max_length {
      Ok(0)
    } else {
      Ok(-1 - self.delegate.byte_at(j, self.d + self.pivot.length())?)
    }
  }
}

impl<T> IntroSelectorBase for IntroSelectorImpl<'_, T>
where
  T: RadixSelectorBase,
{
  fn compare(&mut self, i: usize, j: usize) -> Result<i32> {
    for o in self.d..self.max_length {
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
}
impl<T> Selector for IntroSelectorImpl<'_, T>
where
  T: RadixSelectorBase,
{
  fn swap(&mut self, i: usize, j: usize) -> Result<()> {
    self.delegate.swap(i, j)
  }
}
