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
use crate::core::util::{BINARY_SORT_THRESHOLD, Sorter, check_range};

pub struct InPlaceMergeSorter<S>
where
    S: Sorter,
{
    sub: S,
    pivot_index: i32,
}
impl<S> InPlaceMergeSorter<S>
where
    S: Sorter,
{
    pub fn new(sub: S) -> Self {
        InPlaceMergeSorter {
            sub,
            pivot_index: 0,
        }
    }
    fn merge_sort(&mut self, from: i32, to: i32) -> Result<()> {
        if to - from < BINARY_SORT_THRESHOLD {
            self.binary_sort(from, to)
        } else {
            let mid = (from + to) >> 1;
            self.merge_sort(from, mid)?;
            self.merge_sort(mid, to)?;
            self.merge_in_place(from, mid, to)
        }
    }
}
impl<S> Sorter for InPlaceMergeSorter<S>
where
    S: Sorter,
{
    fn compare(&mut self, i: usize, j: usize) -> Result<i32> {
        self.sub.compare(i, j)
    }

    fn swap(&mut self, i: usize, j: usize) -> Result<()> {
        self.sub.swap(i, j)
    }

    fn set_pivot(&mut self, i: i32) -> Result<()> {
        self.pivot_index = i;
        Ok(())
    }

    fn compare_pivot(&mut self, j: i32) -> Result<i32> {
        self.compare(self.pivot_index as usize, j as usize)
    }

    fn sort(&mut self, from: i32, to: i32) -> Result<()> {
        check_range(from, to)?;
        self.merge_sort(from, to)
    }
}
