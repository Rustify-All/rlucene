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
use crate::core::util::{Comparator, SliceCopyOps, Sorter, TimSorter, TimSorterBase};

/// A [`TimSorter`] for object arrays.
///
/// # Note
/// This is an internal API.
pub struct ArrayTimSorter<'a, T, C: Comparator<T>>
where
    T: Copy,
{
    arr: &'a mut [T],
    tmp: Vec<T>,
    comparator: C,
    pivot_index: i32,
    max_temp_slots: i32,
}
impl<'a, T, C: Comparator<T>> ArrayTimSorter<'a, T, C>
where
    T: Copy,
{
    pub fn new(
        arr: &'a mut [T],
        comparator: C,
        max_temp_slots: i32,
    ) -> TimSorter<ArrayTimSorter<'a, T, C>> {
        let tmp = if max_temp_slots > 0 {
            Vec::with_capacity(max_temp_slots as usize)
        } else {
            vec![]
        };
        let sub = ArrayTimSorter {
            arr,
            tmp,
            comparator,
            pivot_index: 0,
            max_temp_slots,
        };
        TimSorter::new(max_temp_slots, sub)
    }
}
impl<T, C: Comparator<T>> Sorter for ArrayTimSorter<'_, T, C>
where
    T: Copy,
{
    fn compare(&mut self, i: usize, j: usize) -> Result<i32> {
        self.comparator.compare(&self.arr[i], &self.arr[j])
    }

    fn swap(&mut self, i: usize, j: usize) -> Result<()> {
        self.arr.swap(i, j);
        Ok(())
    }

    fn set_pivot(&mut self, i: i32) -> Result<()> {
        self.pivot_index = i;
        Ok(())
    }

    fn compare_pivot(&mut self, j: i32) -> Result<i32> {
        self.compare(self.pivot_index as usize, j as usize)
    }
}
impl<T, C: Comparator<T>> TimSorterBase for ArrayTimSorter<'_, T, C>
where
    T: Copy,
{
    fn copy(&mut self, src: i32, dest: i32) {
        self.arr[dest as usize] = self.arr[src as usize];
    }

    fn save(&mut self, start: i32, len: i32) {
        let tmp_len = self.tmp.len();
        if tmp_len < self.max_temp_slots as usize {
            for _ in 0..(self.max_temp_slots as usize - tmp_len) {
                self.tmp.push(self.arr[start as usize]);
            }
        }
        self.tmp
            .copy_from(&self.arr[start as usize..start as usize + len as usize], 0);
    }

    fn restore(&mut self, src: i32, dest: i32) {
        self.arr[dest as usize] = self.tmp[src as usize];
    }

    fn compare_saved(&self, i: i32, j: i32) -> Result<i32> {
        self.comparator
            .compare(&self.tmp[i as usize], &self.arr[j as usize])
    }
}
