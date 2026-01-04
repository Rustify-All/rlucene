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
use crate::core::util::INSERTION_SORT_THRESHOLD;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::sorter::{Sorter, check_range};

/// Below this size threshold, the partition selection is simplified to a single
/// median.
pub const SINGLE_MEDIAN_THRESHOLD: i32 = 40;

/// [`Sorter`] implementation based on a variant of the quicksort algorithm
/// called [introsort](http://en.wikipedia.org/wiki/Introsort): when the recursion level exceeds the
/// log of the length of the array to sort, it falls back to heapsort. This
/// prevents quicksort from running into its worst-case quadratic runtime.
/// Selects the pivot using Tukey's ninther median-of-medians, and partitions
/// using Bentley-McIlroy 3-way partitioning. Small ranges are sorted with
/// insertion sort.
///
/// # Note
/// This algorithm is **NOT** stable. It's fast on most data shapes, especially
/// with low cardinality. If the data to sort is known to be strictly ascending
/// or descending, prefer [`TimSorter`](crate::core::util::TimSorter).
///
/// # Note
/// This is an internal API.
pub trait IntroSorter: Sorter {
    fn sort_range(&mut self, from: i32, to: i32) -> Result<()> {
        check_range(from, to)?;
        self.sort_in_intro(from, to, (2.0 * ((to - from) as f64).log2()) as usize)?;
        Ok(())
    }
    /// Sorts between `from` (inclusive) and `to` (exclusive) with introsort.
    ///
    /// # Description
    /// Sorts small ranges with insertion sort. Falls back to heapsort to avoid
    /// quadratic worst case. Selects the pivot with medians and partitions
    /// using the Bentley-McIlroy fast 3-way algorithm (Engineering a Sort
    /// Function, Bentley-McIlroy).
    fn sort_in_intro(&mut self, mut from: i32, mut to: i32, mut max_depth: usize) -> Result<()> {
        while to - from > INSERTION_SORT_THRESHOLD {
            if max_depth == 0 {
                // Max recursion depth exceeded: fallback to heap sort.
                self.heap_sort(from, to)?;
                return Ok(());
            }
            max_depth -= 1;

            let size = to - from;
            let last = to - 1;
            let mid = (from + last) >> 1;

            let pivot = if size <= SINGLE_MEDIAN_THRESHOLD {
                // Select the pivot with a single median around the middle
                // element. Do not take the median between
                // [from, mid, last] because it hurts performance
                // if the order is descending in conjunction with the 3-way
                // partitioning.
                let range = size >> 2;
                self.median(mid - range, mid, mid + range)?
            } else {
                // Select the pivot with the Tukey's ninther median of medians.
                let range = size >> 3;
                let double_range = range << 1;
                let median_first = self.median(from, from + range, from + double_range)?;
                let median_middle = self.median(mid - range, mid, mid + range)?;
                let median_last = self.median(last - double_range, last - range, last)?;
                self.median(median_first, median_middle, median_last)?
            };

            self.set_pivot(pivot)?;
            self.swap(from as usize, pivot as usize)?;

            let mut i = from;
            let mut j = to;
            let mut p = from + 1;
            let mut q = last;

            loop {
                let mut left_cmp;

                while {
                    i += 1;
                    left_cmp = self.compare_pivot(i)?;
                    left_cmp > 0
                } {}

                let mut right_cmp;

                while {
                    j -= 1;
                    right_cmp = self.compare_pivot(j)?;
                    right_cmp < 0
                } {}

                if i >= j {
                    if i == j && right_cmp == 0 {
                        self.swap(i as usize, p as usize)?;
                    }
                    break;
                }

                self.swap(i as usize, j as usize)?;
                if right_cmp == 0 {
                    self.swap(i as usize, p as usize)?;
                    p += 1;
                }
                if left_cmp == 0 {
                    self.swap(j as usize, q as usize)?;
                    q -= 1;
                }
            }

            i = j + 1;

            let mut k = from;
            while k < p {
                self.swap(k as usize, j as usize)?;
                k += 1;
                j -= 1;
            }

            k = last;
            while k > q {
                self.swap(k as usize, i as usize)?;
                k -= 1;
                i += 1;
            }
            // Recursion on the smallest partition. Replace the tail recursion
            // by a loop.
            if j - from < last - i {
                self.sort_in_intro(from, j + 1, max_depth)?;
                from = i;
            } else {
                self.sort_in_intro(i, to, max_depth)?;
                to = j + 1;
            }
        }

        self.insertion_sort(from, to)?;
        Ok(())
    }

    /// Returns the index of the median element among three elements at provided
    /// indices.
    fn median(&mut self, i: i32, j: i32, k: i32) -> Result<i32> {
        if self.compare(i as usize, j as usize)? < 0 {
            if self.compare(j as usize, k as usize)? <= 0 {
                return Ok(j);
            }
            return if self.compare(i as usize, k as usize)? < 0 {
                Ok(k)
            } else {
                Ok(i)
            };
        }
        if self.compare(j as usize, k as usize)? >= 0 {
            return Ok(j);
        }
        if self.compare(i as usize, k as usize)? < 0 {
            Ok(i)
        } else {
            Ok(k)
        }
    }
}
