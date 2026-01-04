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
use crate::core::util::comparator::Comparator;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::intro_sorter::IntroSorter;
use crate::core::util::sorter::Sorter;

/// An [`IntroSorter`] for object arrays.
///
/// # Note
/// This is an internal API.
pub(crate) struct ArrayIntroSorter<'a, T, C: Comparator<T>> {
    pub arr: &'a mut [T],
    comparator: C,
    pivot: i32,
}

impl<'a, T, C> ArrayIntroSorter<'a, T, C>
where
    C: Comparator<T>,
{
    pub fn new(arr: &'a mut [T], comparator: C) -> ArrayIntroSorter<'a, T, C> {
        ArrayIntroSorter {
            arr,
            comparator,
            pivot: 0,
        }
    }
}

impl<T, C> Sorter for ArrayIntroSorter<'_, T, C>
where
    C: Comparator<T>,
{
    fn compare(&mut self, i: usize, j: usize) -> Result<i32> {
        self.comparator.compare(&self.arr[i], &self.arr[j])
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
        self.arr.swap(i, j);
        Ok(())
    }

    fn set_pivot(&mut self, i: i32) -> Result<()> {
        self.pivot = i;
        Ok(())
    }

    fn compare_pivot(&mut self, i: i32) -> Result<i32> {
        self.comparator
            .compare(&self.arr[self.pivot as usize], &self.arr[i as usize])
    }

    fn sort(&mut self, from: i32, to: i32) -> Result<()> {
        IntroSorter::sort_range(self, from, to)?;
        Ok(())
    }
}

impl<T, C: Comparator<T>> IntroSorter for ArrayIntroSorter<'_, T, C> {}

#[cfg(test)]
mod tests {

    use rand::Rng;

    use crate::core::util::{ArrayIntroSorter, Comparator, NaturalOrder, Sorter};
    use crate::test::util::base_sort_test_case::{BaseSortTestCase, Entry};
    use crate::test::util::lucene_test_case::lucene_test_case_util::random;

    const STABLE: bool = false;

    struct TestIntroSorter<T, C: Comparator<T>> {
        _marker: std::marker::PhantomData<(T, C)>,
    }
    impl Default for TestIntroSorter<i32, NaturalOrder> {
        fn default() -> Self {
            TestIntroSorter {
                _marker: std::marker::PhantomData,
            }
        }
    }

    impl<T, C: Comparator<T>> BaseSortTestCase for TestIntroSorter<T, C> {
        fn new_sorter<R: Rng + ?Sized>(
            &self,
            _random: &mut R,
            arr: &mut Vec<Entry>,
        ) -> impl Sorter {
            ArrayIntroSorter::new(arr, NaturalOrder::new())
        }

        fn get_stable(&self) -> bool {
            STABLE
        }
    }

    #[test]
    fn test_empty() {
        let mut random = random();
        let case = TestIntroSorter::default();
        case.test_empty(&mut random);
    }
    #[test]
    fn test_one() {
        let mut random = random();
        let case = TestIntroSorter::default();
        case.test_one(&mut random);
    }
    #[test]
    fn test_two() {
        let mut random = random();
        let case = TestIntroSorter::default();
        case.test_two(&mut random);
    }
    #[test]
    fn test_random() {
        let mut random = random();
        let case = TestIntroSorter::default();
        case.test_random(&mut random);
    }
    #[test]
    fn test_random_low_cardinality() {
        let mut random = random();
        let case = TestIntroSorter::default();
        case.test_random_low_cardinality(&mut random);
    }
    #[test]
    fn test_ascending() {
        let mut random = random();
        let case = TestIntroSorter::default();
        case.test_ascending(&mut random);
    }
    #[test]
    fn test_ascending_sequences() {
        let mut random = random();
        let case = TestIntroSorter::default();
        case.test_ascending_sequences(&mut random);
    }
    #[test]
    fn test_descending() {
        let mut random = random();
        let case = TestIntroSorter::default();
        case.test_descending(&mut random);
    }
    #[test]
    fn test_strictly_descending() {
        let mut random = random();
        let case = TestIntroSorter::default();
        case.test_strictly_descending(&mut random);
    }
}
