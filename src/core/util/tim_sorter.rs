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

use std::cmp::{max, min};

use crate::core::util::error::lucene_error::Result;
use crate::core::util::{Sorter, sorter};

const MIN_RUN: i32 = 32;
const THRESHOLD: i32 = 64;
const STACK_SIZE: i32 = 49; // depends on MINRUN
const MIN_GALLOP: i32 = 7;

/// [`Sorter`] implementation based on the [TimSort](http://svn.python.org/projects/python/trunk/Objects/listsort.txt) algorithm. It
/// sorts small arrays with a binary sort.
///
/// This algorithm is stable and is especially good at sorting partially-sorted
/// arrays.
///
/// # Note
/// There are a few differences with the original implementation:
/// - The extra amount of memory to perform merges is configurable. This allows
///   small merges to be very fast, while large merges will be performed
///   in-place (slightly slower). You can ensure that the fast merge routine
///   will always be used by having `max_temp_slots` equal to half the length of
///   the slice of data to sort.
/// - Only the fast merge routine can gallop (the one that doesn't run
///   in-place), and it only gallops on the longest slice.
///
/// # Note
/// This is an internal API.
pub struct TimSorter<T>
where
    T: TimSorterBase,
{
    max_temp_slots: i32,
    min_run: i32,
    to: i32,
    stack_size: i32,
    run_ends: Vec<i32>,
    delegate: T,
}
impl<T: TimSorterBase> TimSorter<T> {
    pub fn new(max_temp_slots: i32, delegate: T) -> TimSorter<T> {
        TimSorter {
            max_temp_slots,
            min_run: 0,
            to: 0,
            stack_size: 0,
            run_ends: vec![0; STACK_SIZE as usize + 1],
            delegate,
        }
    }
    fn min_run(&self, length: i32) -> i32 {
        debug_assert!(length >= MIN_RUN);
        let mut n = length;
        let mut r = 0;
        while n >= 64 {
            r |= n & 1;
            n >>= 1;
        }
        let min_run = n + r;
        debug_assert!((MIN_RUN..=THRESHOLD).contains(&min_run));
        min_run
    }
    fn run_len(&self, i: i32) -> i32 {
        let off = self.stack_size - i;
        self.run_ends[off as usize] - self.run_ends[(off - 1) as usize]
    }
    fn run_base(&self, i: i32) -> i32 {
        self.run_ends[(self.stack_size - i - 1) as usize]
    }
    fn run_end(&self, i: i32) -> i32 {
        self.run_ends[(self.stack_size - i) as usize]
    }
    fn set_run_end(&mut self, i: i32, run_end: i32) {
        self.run_ends[(self.stack_size - i) as usize] = run_end;
    }
    fn push_run_len(&mut self, len: i32) {
        self.run_ends[(self.stack_size + 1) as usize] =
            self.run_ends[self.stack_size as usize] + len;
        self.stack_size += 1;
    }
    // Compute the length of the next run, make the run sorted and return its
    // length.
    fn next_run(&mut self) -> Result<i32> {
        let run_base = self.run_end(0);
        debug_assert!(run_base < self.to);

        if run_base == self.to - 1 {
            return Ok(1);
        }
        let mut o = run_base + 2;
        if self.compare(run_base as usize, (run_base + 1) as usize)? > 0 {
            while o < self.to && self.compare((o - 1) as usize, o as usize)? > 0 {
                o += 1;
            }
            self.reverse(run_base, o)?;
        } else {
            while o < self.to && self.compare((o - 1) as usize, o as usize)? <= 0 {
                o += 1;
            }
        }
        let run_hi = max(o, min(self.to, run_base + self.min_run));
        self.binary_sort_with_start(run_base, run_hi, o)?;
        Ok(run_hi - run_base)
    }
    pub fn ensure_invariants(&mut self) -> Result<()> {
        while self.stack_size > 1 {
            let run_len0 = self.run_len(0);
            let run_len1 = self.run_len(1);

            if self.stack_size > 2 {
                let run_len2 = self.run_len(2);

                if run_len2 <= run_len1 + run_len0 {
                    // merge the smaller of 0 and 2 with 1
                    if run_len2 < run_len0 {
                        self.merge_at(1)?;
                    } else {
                        self.merge_at(0)?;
                    }
                    continue;
                }
            }

            if run_len1 <= run_len0 {
                self.merge_at(0)?;
                continue;
            }

            break;
        }
        Ok(())
    }
    pub fn exhaust_stack(&mut self) -> Result<()> {
        while self.stack_size > 1 {
            self.merge_at(0)?;
        }
        Ok(())
    }

    pub fn reset(&mut self, from: i32, to: i32) {
        self.stack_size = 0;
        self.run_ends.fill(0);
        self.run_ends[0] = from;
        self.to = to;
        let length = to - from;
        self.min_run = if length <= THRESHOLD {
            length
        } else {
            self.min_run(length)
        };
    }

    pub fn merge_at(&mut self, n: i32) -> Result<()> {
        debug_assert!(self.stack_size >= 2);
        self.merge(self.run_base(n + 1), self.run_base(n), self.run_end(n))?;

        for j in (1..=n + 1).rev() {
            self.set_run_end(j, self.run_end(j - 1));
        }

        self.stack_size -= 1;
        Ok(())
    }

    fn merge(&mut self, mut lo: i32, mid: i32, mut hi: i32) -> Result<()> {
        if self.compare((mid - 1) as usize, mid as usize)? <= 0 {
            return Ok(());
        }

        lo = self.upper2(lo, mid, mid)?;
        hi = self.lower2(mid, hi, mid - 1)?;
        if hi - mid <= mid - lo && hi - mid <= self.max_temp_slots {
            self.merge_hi(lo, mid, hi)?;
        } else if mid - lo <= self.max_temp_slots {
            self.merge_lo(lo, mid, hi)?;
        } else {
            self.merge_in_place(lo, mid, hi)?;
        }
        Ok(())
    }

    fn merge_lo(&mut self, lo: i32, mid: i32, hi: i32) -> Result<()> {
        debug_assert!(self.delegate.compare(lo as usize, mid as usize)? > 0);

        let len1 = mid - lo;
        self.delegate.save(lo, len1);
        self.delegate.copy(mid, lo);

        let mut i = 0;
        let mut j = mid + 1;
        let mut dest = lo + 1;

        'outer: loop {
            let mut count = 0;
            while count < MIN_GALLOP {
                if i >= len1 || j >= hi {
                    break 'outer;
                } else if self.delegate.compare_saved(i, j)? <= 0 {
                    self.delegate.restore(i, dest);
                    i += 1;
                    dest += 1;
                    count = 0;
                } else {
                    self.delegate.copy(j, dest);
                    j += 1;
                    dest += 1;
                    count += 1;
                }
            }

            // Galloping phase
            let next = self.lower_saved3(j, hi, i)?;
            while j < next {
                self.delegate.copy(j, dest);
                j += 1;
                dest += 1;
            }
            self.delegate.restore(i, dest);
            i += 1;
            dest += 1;
        }

        while i < len1 {
            self.delegate.restore(i, dest);
            i += 1;
            dest += 1;
        }

        debug_assert_eq!(j, dest);
        Ok(())
    }

    pub fn merge_hi(&mut self, lo: i32, mid: i32, hi: i32) -> Result<()> {
        debug_assert!(
            self.compare((mid - 1) as usize, (hi - 1) as usize)? > 0
        );

        let len2 = hi - mid;
        self.delegate.save(mid, len2);
        self.delegate.copy(mid - 1, hi - 1);

        let mut i = mid - 2;
        let mut j: i32 = len2 - 1;
        let mut dest = hi - 2;

        'outer: loop {
            let mut count = 0;
            while count < MIN_GALLOP {
                if i < lo || j < 0 {
                    break 'outer;
                } else if self.delegate.compare_saved(j, i)? >= 0 {
                    self.delegate.restore(j, dest);
                    j -= 1;
                    dest -= 1;
                    count = 0;
                } else {
                    self.delegate.copy(i, dest);
                    i -= 1;
                    dest -= 1;
                    count += 1;
                }
            }

            // Galloping phase
            let next = self.upper_saved3(lo, i + 1, j)?;
            while i >= next {
                self.delegate.copy(i, dest);
                i -= 1;
                dest -= 1;
            }
            self.delegate.restore(j, dest);
            j -= 1;
            dest -= 1;
        }

        while j >= 0 {
            self.delegate.restore(j, dest);
            j -= 1;
            dest -= 1;
        }

        debug_assert!(i == dest);
        Ok(())
    }

    pub fn lower_saved(&self, mut from: i32, to: i32, val: i32) -> Result<i32> {
        let mut len = to - from;

        while len > 0 {
            let half = len >> 1;
            let mid = from + half;
            if self.delegate.compare_saved(val, mid)? > 0 {
                from = mid + 1;
                len -= half + 1;
            } else {
                len = half;
            }
        }
        Ok(from)
    }

    pub fn upper_saved(&self, mut from: i32, to: i32, val: i32) -> Result<i32> {
        let mut len = to - from;

        while len > 0 {
            let half = len >> 1;
            let mid = from + half;
            if self.delegate.compare_saved(val, mid)? < 0 {
                len = half;
            } else {
                from = mid + 1;
                len -= half + 1;
            }
        }
        Ok(from)
    }

    pub fn lower_saved3(&self, from: i32, to: i32, val: i32) -> Result<i32> {
        let mut f = from;
        let mut t = f + 1;

        while t < to {
            if self.delegate.compare_saved(val, t)? <= 0 {
                return self.lower_saved(f, t, val);
            }
            let delta = t - f;
            f = t;
            t += delta * 2;
        }
        self.lower_saved(f, to, val)
    }

    pub fn upper_saved3(&self, from: i32, to: i32, val: i32) -> Result<i32> {
        let mut f = to - 1;
        let mut t = to;

        while f > from {
            if self.delegate.compare_saved(val, f)? >= 0 {
                return self.upper_saved(f, t, val);
            }
            let delta = t - f;
            t = f;
            f -= delta * 2
        }
        self.upper_saved(from, t, val)
    }
}
impl<T> Sorter for TimSorter<T>
where
    T: TimSorterBase,
{
    fn compare(&mut self, i: usize, j: usize) -> Result<i32> {
        self.delegate.compare(i, j)
    }

    fn swap(&mut self, i: usize, j: usize) -> Result<()> {
        self.delegate.swap(i, j)
    }

    fn set_pivot(&mut self, i: i32) -> Result<()> {
        self.delegate.set_pivot(i)
    }

    fn compare_pivot(&mut self, i: i32) -> Result<i32> {
        self.delegate.compare_pivot(i)
    }

    fn sort(&mut self, from: i32, to: i32) -> Result<()> {
        sorter::check_range(from, to)?;
        if to - from <= 1 {
            return Ok(());
        }

        self.reset(from, to);

        loop {
            self.ensure_invariants()?;
            let run_length = self.next_run()?;
            self.push_run_len(run_length);

            if self.run_end(0) >= to {
                break;
            }
        }
        self.exhaust_stack()?;

        debug_assert_eq!(self.run_end(0), to);
        Ok(())
    }

    fn do_rotate(&mut self, mut lo: i32, mut mid: i32, hi: i32) -> Result<()> {
        let len1 = mid - lo;
        let len2 = hi - mid;

        if len1 == len2 {
            while mid < hi {
                self.swap(lo as usize, mid as usize)?;
                lo += 1;
                mid += 1;
            }
        } else if len2 < len1 && len2 <= self.max_temp_slots {
            self.delegate.save(mid, len2);
            let mut i = lo + len1 - 1;
            let mut j = hi - 1;
            while i >= lo {
                self.delegate.copy(i, j);
                i -= 1;
                j -= 1;
            }
            i = 0;
            j = lo;
            while i < len2 {
                self.delegate.restore(i, j);
                i += 1;
                j += 1;
            }
        } else if len1 <= self.max_temp_slots {
            self.delegate.save(lo, len1);
            let mut i = mid;
            let mut j = lo;
            while i < hi {
                self.delegate.copy(i, j);
                i += 1;
                j += 1;
            }
            i = 0;
            j = lo + len2;
            while j < hi {
                self.delegate.restore(i, j);
                i += 1;
                j += 1;
            }
        } else {
            self.reverse(lo, mid)?;
            self.reverse(mid, hi)?;
            self.reverse(lo, hi)?;
        }
        Ok(())
    }
}

pub trait TimSorterBase: Sorter {
    ///Copy data from slot `src` to slot `dest`
    fn copy(&mut self, src: i32, dest: i32);

    /// Save all elements between slots i and `i+len` into the temporary
    /// storage.
    fn save(&mut self, i: i32, len: i32);
    /// Restore element `j` from the temporary storage into slot `i`.
    fn restore(&mut self, i: i32, j: i32);

    /// Compare element `i` from the temporary storage with element `j` from the
    /// slice to sort, similarly to #compare(i32, i32).
    fn compare_saved(&self, i: i32, j: i32) -> Result<i32>;
}

#[cfg(test)]
mod tests {
    use rand::Rng;

    use crate::core::util::array_tim_sorter::ArrayTimSorter;
    use crate::core::util::{NaturalOrder, Sorter};
    use crate::test::util::base_sort_test_case::{BaseSortTestCase, Entry};
    use crate::test::util::lucene_test_case::lucene_test_case_util::random;
    use crate::test::util::test_util::TestUtil;

    struct TestTimSorter;

    impl TestTimSorter {
        fn default() -> Self {
            TestTimSorter {}
        }
    }

    impl BaseSortTestCase for TestTimSorter {
        fn new_sorter<R: Rng + ?Sized>(&self, random: &mut R, arr: &mut Vec<Entry>) -> impl Sorter {
            let arr_len = arr.len();
            let max_temp_slots = TestUtil::next_int(random, 0, arr_len as i32);
            ArrayTimSorter::new(arr, NaturalOrder::new(), max_temp_slots)
        }

        fn get_stable(&self) -> bool {
            true
        }
    }

    #[test]
    fn test_empty() {
        let mut random = random();
        let case = TestTimSorter::default();
        case.test_empty(&mut random);
    }
    #[test]
    fn test_one() {
        let mut random = random();
        let case = TestTimSorter::default();
        case.test_one(&mut random);
    }
    #[test]
    fn test_two() {
        let mut random = random();
        let case = TestTimSorter::default();
        case.test_two(&mut random);
    }
    #[test]
    fn test_random() {
        let mut random = random();
        let case = TestTimSorter::default();
        case.test_random(&mut random);
    }
    #[test]
    fn test_random_low_cardinality() {
        let mut random = random();
        let case = TestTimSorter::default();
        case.test_random_low_cardinality(&mut random);
    }
    #[test]
    fn test_ascending() {
        let mut random = random();
        let case = TestTimSorter::default();
        case.test_ascending(&mut random);
    }
    #[test]
    fn test_ascending_sequences() {
        let mut random = random();
        let case = TestTimSorter::default();
        case.test_ascending_sequences(&mut random);
    }
    #[test]
    fn test_descending() {
        let mut random = random();
        let case = TestTimSorter::default();
        case.test_descending(&mut random);
    }
    #[test]
    fn test_strictly_descending() {
        let mut random = random();
        let case = TestTimSorter::default();
        case.test_strictly_descending(&mut random);
    }
}
