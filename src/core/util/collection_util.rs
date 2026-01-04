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
use crate::core::util::intro_sorter::IntroSorter;
use crate::core::util::{Comparator, NaturalOrder, SliceCopyOps, Sorter, TimSorter, TimSorterBase};
use std::collections::HashMap;

///  Methods for manipulating (sorting) and creating collections. Sort methods work directly on the supplied lists and don't copy to/from arrays before/after. For medium size collections as used in the Lucene indexer that is much more efficient.
pub struct CollectionUtil;

impl CollectionUtil {
    #[allow(dead_code)]
    pub fn new_hashmap<K, V>(size: usize) -> HashMap<K, V> {
        let capacity = ((size as f32) / 0.75f32 + 1f32) as usize;
        HashMap::with_capacity(capacity)
    }
    /// Sorts the given random access `List` using the `Comparator`.
    pub fn intro_sort_with_comparator<T, C: Comparator<T>>(list: &mut [T], comp: C) -> Result<()> {
        let size = list.len();
        if size <= 1 {
            return Ok(());
        }
        ListIntroSorter::new(list, comp).sort(0, size as i32)?;
        Ok(())
    }
    pub fn intro_sort<T>(list: &mut [T]) -> Result<()>
    where
        T: Ord,
    {
        let size = list.len();
        if size <= 1 {
            return Ok(());
        }
        Self::intro_sort_with_comparator(list, NaturalOrder::new())
    }

    pub fn tim_sort_with_comparator<T, C: Comparator<T>>(list: &mut [T], comp: C) -> Result<()>
    where
        T: Copy,
    {
        let size = list.len();
        if size <= 1 {
            return Ok(());
        }
        ListTimSorter::new(list, comp, size as i32 / 64).sort(0, size as i32)?;
        Ok(())
    }
    pub fn tim_sort<T>(list: &mut [T]) -> Result<()>
    where
        T: Copy + Ord,
    {
        let size = list.len();
        if size <= 1 {
            return Ok(());
        }
        Self::tim_sort_with_comparator(list, NaturalOrder::new())
    }
}
// ListTimSorter
struct ListTimSorter<'a, T, C: Comparator<T>>
where
    T: Copy,
{
    arr: &'a mut [T],
    tmp: Vec<T>,
    comp: C,
    pivot: i32,
    max_temp_slots: i32,
}
impl<'a, T, C: Comparator<T>> ListTimSorter<'a, T, C>
where
    T: Copy,
{
    pub fn new(
        arr: &'a mut [T],
        comp: C,
        max_temp_slots: i32,
    ) -> TimSorter<ListTimSorter<'a, T, C>> {
        let tmp = if max_temp_slots > 0 {
            Vec::with_capacity(max_temp_slots as usize)
        } else {
            vec![]
        };
        let sub = ListTimSorter {
            arr,
            tmp,
            comp,
            pivot: 0,
            max_temp_slots,
        };
        TimSorter::new(max_temp_slots, sub)
    }
}
impl<T, C: Comparator<T>> Sorter for ListTimSorter<'_, T, C>
where
    T: Copy,
{
    fn compare(&mut self, i: usize, j: usize) -> Result<i32> {
        self.comp.compare(&self.arr[i], &self.arr[j])
    }

    fn swap(&mut self, i: usize, j: usize) -> Result<()> {
        self.arr.swap(i, j);
        Ok(())
    }

    fn set_pivot(&mut self, i: i32) -> Result<()> {
        self.pivot = i;
        Ok(())
    }

    fn compare_pivot(&mut self, j: i32) -> Result<i32> {
        self.compare(self.pivot as usize, j as usize)
    }
}
impl<T, C: Comparator<T>> TimSorterBase for ListTimSorter<'_, T, C>
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
        self.comp
            .compare(&self.tmp[i as usize], &self.arr[j as usize])
    }
}

// ListIntroSorter
struct ListIntroSorter<'a, T, C: Comparator<T>> {
    pub list: &'a mut [T],
    comp: C,
    pivot: i32,
}
impl<'a, T, C> ListIntroSorter<'a, T, C>
where
    C: Comparator<T>,
{
    pub fn new(list: &'a mut [T], comp: C) -> ListIntroSorter<'a, T, C> {
        ListIntroSorter {
            list,
            comp,
            pivot: 0,
        }
    }
}

impl<T, C: Comparator<T>> Sorter for ListIntroSorter<'_, T, C> {
    fn compare(&mut self, i: usize, j: usize) -> Result<i32> {
        self.comp.compare(&self.list[i], &self.list[j])
    }

    fn swap(&mut self, i: usize, j: usize) -> Result<()> {
        // The data pointed to by the pivot has been swapped.
        // We need to adjust the pivot value to ensure that
        // the value corresponding to the pivot remains unchanged.
        // To avoid Copying the value, we just swap the pivot index.
        let pivot = self.pivot as usize;
        if pivot == i || pivot == j {
            self.pivot = if pivot == i { j } else { i } as i32;
        }
        self.list.swap(i, j);
        Ok(())
    }

    fn set_pivot(&mut self, i: i32) -> Result<()> {
        self.pivot = i;
        Ok(())
    }

    fn compare_pivot(&mut self, i: i32) -> Result<i32> {
        self.comp
            .compare(&self.list[self.pivot as usize], &self.list[i as usize])
    }

    fn sort(&mut self, from: i32, to: i32) -> Result<()> {
        IntroSorter::sort_range(self, from, to)?;
        Ok(())
    }
}

impl<T, C: Comparator<T>> IntroSorter for ListIntroSorter<'_, T, C> {}

#[cfg(test)]
mod tests {
    use crate::core::util::ReverseOrder;
    use crate::core::util::collection_util::CollectionUtil;
    use crate::core::util::error::lucene_error::Result;
    use crate::test::util::lucene_test_case::lucene_test_case_util::{at_least, random};
    use rand::Rng;

    fn create_random_list<R: Rng + ?Sized>(random: &mut R, max_size: usize) -> Vec<i32> {
        let len = random.random_range(1..=max_size);
        (0..len)
            .map(|_| random.random_range(0..len as i32))
            .collect()
    }
    #[test]
    fn test_intro_sort() -> Result<()> {
        let mut random = random();
        for _ in 0..at_least(&mut random, 100) {
            let mut list1 = create_random_list(&mut random, 2000);
            let mut list2 = list1.clone();
            CollectionUtil::intro_sort(&mut list1)?;
            list2.sort();
            assert_eq!(list1, list2);
            let mut list1 = create_random_list(&mut random, 2000);
            let mut list2 = list1.clone();
            CollectionUtil::intro_sort_with_comparator(&mut list1, ReverseOrder::new())?;
            list2.sort_by(|a, b| b.cmp(a));
            assert_eq!(list1, list2);
            CollectionUtil::intro_sort(&mut list1)?;
            list2.sort();
            assert_eq!(list1, list2);
        }

        Ok(())
    }

    #[test]
    fn test_tim_sort() -> Result<()> {
        let mut random = random();
        for _ in 0..at_least(&mut random, 100) {
            let mut list1 = create_random_list(&mut random, 2000);
            let mut list2 = list1.clone();
            CollectionUtil::tim_sort(&mut list1)?;
            list2.sort();
            assert_eq!(list1, list2);

            let mut list1 = create_random_list(&mut random, 2000);
            let mut list2 = list1.clone();
            CollectionUtil::tim_sort_with_comparator(&mut list1, ReverseOrder::new())?;
            list2.sort_by(|a, b| b.cmp(a));
            assert_eq!(list1, list2);

            CollectionUtil::tim_sort(&mut list1)?;
            list2.sort();
            assert_eq!(list1, list2);
        }

        Ok(())
    }
    #[test]
    fn test_empty_list_sort() -> Result<()> {
        let mut vec: Vec<i32> = Vec::new();
        CollectionUtil::intro_sort(&mut vec)?;
        CollectionUtil::tim_sort(&mut vec)?;
        CollectionUtil::intro_sort_with_comparator(&mut vec, ReverseOrder::new())?;
        CollectionUtil::tim_sort_with_comparator(&mut vec, ReverseOrder::new())?;

        use std::collections::VecDeque;

        let list: VecDeque<i32> = VecDeque::new();
        let mut vec2: Vec<i32> = list.into_iter().collect();
        CollectionUtil::intro_sort(&mut vec2)?;
        CollectionUtil::tim_sort(&mut vec2)?;
        CollectionUtil::intro_sort_with_comparator(&mut vec2, ReverseOrder::new())?;
        CollectionUtil::tim_sort_with_comparator(&mut vec2, ReverseOrder::new())?;

        Ok(())
    }

    #[test]
    fn test_one_element_list_sort() -> Result<()> {
        let mut list = Vec::new();
        list.push(1);
        CollectionUtil::intro_sort(&mut list)?;
        CollectionUtil::tim_sort(&mut list)?;
        CollectionUtil::intro_sort_with_comparator(&mut list, ReverseOrder::new())?;
        CollectionUtil::tim_sort_with_comparator(&mut list, ReverseOrder::new())?;
        Ok(())
    }
}
