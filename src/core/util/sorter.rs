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
use crate::core::util::error::lucene_error::{LuceneError, Result};

/// Base struct for sorting algorithm implementations.
///
/// There are a number of SubStruct to choose from that vary in performance and [stability](https://en.wikipedia.org/wiki/Sorting_algorithm#Stability).
/// We suggest that you pick the first from this ranked list that meets your
/// requirements:
///
/// 1. [`MSBRadixSorter`](crate::core::util::MSBRadixSorter) for strings (array of
///    bytes/chars). Not a stable sort.
/// 2. [`StableMSBRadixSorter`](crate::core::util::StableMSBRadixSorter) for strings
///    (array of bytes/chars). Stable sort.
/// 3. [`IntroSorter`](crate::core::util::intro_sorter::IntroSorter). Not a stable
///    sort.
/// 4. [`InPlaceMergeSorter`](crate::core::util::in_place_merge_sorter::InPlaceMergeSorter). When the data to sort is typically small. Stable sort.
/// 5. [`TimSorter`](crate::core::util::tim_sorter::TimSorter). Stable sort.
///
/// # Note
/// This is an internal API.
pub trait Sorter {
    /// Compare entries found in slots i and j
    fn compare(&mut self, _i: usize, _j: usize) -> Result<i32> {
        Err(LuceneError::illegal_state(
            "compare() must be implemented if it needs to be used",
        ))
    }

    /// Swap values at slots <code>i</code> and `j`.
    fn swap(&mut self, _i: usize, _j: usize) -> Result<()> {
        Err(LuceneError::illegal_state(
            "swap() must be implemented if it needs to be used",
        ))
    }

    /// Save the value at slot i so that it can later be used as a pivot, see
    /// `comparePivot(i32)`.
    fn set_pivot(&mut self, _i: i32) -> Result<()> {
        Err(LuceneError::illegal_state(
            "set_pivot() must be implemented if it needs to be used",
        ))
    }

    /// Compare the pivot with the slot at j, similarly to `#compare(i32, i32)`
    /// compare(i, j).
    fn compare_pivot(&mut self, _i: i32) -> Result<i32> {
        Err(LuceneError::illegal_state(
            "compare_pivot() must be implemented if it needs to be used",
        ))
    }

    /// Sort the slice which starts at `from` (inclusive) and ends at `to`
    /// (exclusive).
    fn sort(&mut self, _from: i32, _to: i32) -> Result<()> {
        Err(LuceneError::illegal_state(
            "sort() must be implemented if it needs to be used",
        ))
    }

    fn merge_in_place(&mut self, mut from: i32, mid: i32, mut to: i32) -> Result<()> {
        if from == mid || mid == to || self.compare((mid - 1) as usize, mid as usize)? <= 0 {
            return Ok(());
        } else if to - from == 2 {
            self.swap((mid - 1) as usize, mid as usize)?;
            return Ok(());
        }

        while self.compare(from as usize, mid as usize)? <= 0 {
            from += 1;
        }
        while self.compare((mid - 1) as usize, (to - 1) as usize)? <= 0 {
            to -= 1;
        }

        let (first_cut, second_cut, _len11, len22) = if mid - from > to - mid {
            let len11 = (mid - from) >> 1;
            let first_cut = from + len11;
            let second_cut = self.lower(mid, to, first_cut)?;
            let len22 = second_cut - mid;
            (first_cut, second_cut, len11, len22)
        } else {
            let len22 = (to - mid) >> 1;
            let second_cut = mid + len22;
            let first_cut = self.upper(from, mid, second_cut)?;
            let len11 = first_cut - from;
            (first_cut, second_cut, len11, len22)
        };

        self.rotate(first_cut, mid, second_cut)?;
        let new_mid = first_cut + len22;
        self.merge_in_place(from, first_cut, new_mid)?;
        self.merge_in_place(new_mid, second_cut, to)?;
        Ok(())
    }

    fn lower(&mut self, mut from: i32, to: i32, val: i32) -> Result<i32> {
        let mut len = to - from;
        while len > 0 {
            let half = len >> 1;
            let mid = from + half;
            if self.compare(mid as usize, val as usize)? < 0 {
                from = mid + 1;
                len = len - half - 1;
            } else {
                len = half;
            }
        }
        Ok(from)
    }

    fn upper(&mut self, mut from: i32, to: i32, val: i32) -> Result<i32> {
        let mut len = to - from;
        while len > 0 {
            let half = len >> 1;
            let mid = from + half;
            if self.compare(val as usize, mid as usize)? < 0 {
                len = half;
            } else {
                from = mid + 1;
                len = len - half - 1;
            }
        }
        Ok(from)
    }
    // faster than lower when val is at the end of [from:to[
    fn lower2(&mut self, from: i32, to: i32, val: i32) -> Result<i32> {
        let mut f = to - 1;
        let mut t = to;

        while f > from {
            if self.compare(f as usize, val as usize)? < 0 {
                return self.lower(f, t, val);
            }
            let delta = t - f;
            t = f;
            f -= delta << 1
        }

        self.lower(from, t, val)
    }

    // faster than upper when val is at the beginning of [from:to[
    fn upper2(&mut self, from: i32, to: i32, val: i32) -> Result<i32> {
        let mut f = from;
        let mut t = f + 1;

        while t < to {
            if self.compare(t as usize, val as usize)? > 0 {
                return self.upper(f, t, val);
            }
            let delta = t - f;
            f = t;
            t += delta << 1;
        }

        self.upper(f, to, val)
    }

    fn reverse(&mut self, mut from: i32, to: i32) -> Result<()> {
        let mut to = to - 1;
        while from < to {
            self.swap(from as usize, to as usize)?;
            from += 1;
            to -= 1;
        }
        Ok(())
    }

    fn rotate(&mut self, lo: i32, mid: i32, hi: i32) -> Result<()> {
        debug_assert!(lo <= mid && mid <= hi);
        if lo == mid || mid == hi {
            return Ok(());
        }
        self.do_rotate(lo, mid, hi)
    }

    fn do_rotate(&mut self, mut lo: i32, mut mid: i32, hi: i32) -> Result<()> {
        if mid - lo == hi - mid {
            while mid < hi {
                self.swap(lo as usize, mid as usize)?;
                lo += 1;
                mid += 1;
            }
        } else {
            self.reverse(lo, mid)?;
            self.reverse(mid, hi)?;
            self.reverse(lo, hi)?;
        }
        Ok(())
    }

    /// A binary sort implementation.
    ///
    /// This sorting algorithm performs `O(n * log(n))` comparisons and `O(n^2)`
    /// swaps. It is typically used as a fallback by more sophisticated
    /// sorting implementations when the number of items to sort becomes
    /// smaller than `BINARY_SORT_THRESHOLD`.
    ///
    /// This algorithm is **stable**.
    fn binary_sort(&mut self, from: i32, to: i32) -> Result<()> {
        self.binary_sort_with_start(from, to, from + 1)
    }

    fn binary_sort_with_start(&mut self, from: i32, to: i32, mut i: i32) -> Result<()> {
        while i < to {
            self.set_pivot(i)?;
            let mut l = from;
            let mut h = i - 1;
            while l <= h {
                let mid = (l + h) >> 1;
                let cmp = self.compare_pivot(mid)?;
                if cmp < 0 {
                    h = mid - 1;
                } else {
                    l = mid + 1;
                }
            }
            let mut j = i;
            while j > l {
                self.swap((j - 1) as usize, j as usize)?;
                j -= 1;
            }
            i += 1;
        }
        Ok(())
    }

    /// Sorts between `from` (inclusive) and `to` (exclusive) with insertion
    /// sort. Runs in `O(n^2)`. It is typically used by more sophisticated
    /// implementations as a fall-back when the number of items to sort
    /// becomes less than `INSERTION_SORT_THRESHOLD`. This algorithm is stable.
    fn insertion_sort(&mut self, from: i32, to: i32) -> Result<()> {
        let mut i = from + 1;
        while i < to {
            let mut current = i;
            i += 1;
            loop {
                let previous = current - 1;
                if self.compare(previous as usize, current as usize)? > 0 {
                    self.swap(previous as usize, current as usize)?;
                    if previous == from {
                        break;
                    }
                    current = previous;
                } else {
                    break;
                }
            }
        }
        Ok(())
    }
    /// Uses heap sort to sort items between `from` (inclusive) and `to`
    /// (exclusive). This runs in `O(n * log(n))` and is used as a fall-back
    /// by [`IntroSorter`](crate::core::util::intro_sorter). This algorithm is NOT
    /// stable.
    fn heap_sort(&mut self, from: i32, to: i32) -> Result<()> {
        if to - from <= 1 {
            return Ok(());
        }
        self.heapify(from, to)?;
        let mut end = to - 1;
        while end > from {
            self.swap(from as usize, end as usize)?;
            self.sift_down(from, from, end)?;
            end -= 1;
        }
        Ok(())
    }

    fn heapify(&mut self, from: i32, to: i32) -> Result<()> {
        let mut i = Self::heap_parent(from, to - 1);
        while i >= from {
            self.sift_down(i, from, to)?;
            i -= 1;
        }
        Ok(())
    }

    fn sift_down(&mut self, mut i: i32, from: i32, to: i32) -> Result<()> {
        let mut left_child = Self::heap_child(from, i);
        while left_child < to {
            let right_child = left_child + 1;
            if self.compare(i as usize, left_child as usize)? < 0 {
                if right_child < to && self.compare(left_child as usize, right_child as usize)? < 0
                {
                    self.swap(i as usize, right_child as usize)?;
                    i = right_child;
                } else {
                    self.swap(i as usize, left_child as usize)?;
                    i = left_child;
                }
            } else if right_child < to && self.compare(i as usize, right_child as usize)? < 0 {
                self.swap(i as usize, right_child as usize)?;
                i = right_child;
            } else {
                break;
            }
            left_child = Self::heap_child(from, i);
        }
        Ok(())
    }
    fn heap_parent(from: i32, i: i32) -> i32 {
        ((i - 1 - from) >> 1) + from
    }

    fn heap_child(from: i32, i: i32) -> i32 {
        ((i - from) << 1) + 1 + from
    }
}
pub fn check_range(from: i32, to: i32) -> Result<()> {
    if to < from {
        return Err(LuceneError::illegal_argument(format!(
            "'to' must be >= 'from', got from= {from} and to= {to}"
        )));
    }
    Ok(())
}

pub(crate) const BINARY_SORT_THRESHOLD: i32 = 20;
// Below this size threshold, the sub-range is sorted using Insertion sort.
pub(crate) const INSERTION_SORT_THRESHOLD: i32 = 16;
