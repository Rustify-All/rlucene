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
    BINARY_SORT_THRESHOLD, HISTOGRAM_SIZE, MSBRadixSorterBase, SliceCopyOps, Sorter, check_range,
};

pub struct StableMSBRadixSorter<T>
where
    T: StableMSBRadixSorterBase,
{
    delegate: T,
    fixed_start_offsets: Vec<i32>,
    max_length: usize,
}

impl<T> StableMSBRadixSorter<T>
where
    T: StableMSBRadixSorterBase,
{
    pub fn new(delegate: T, max_length: usize) -> StableMSBRadixSorter<T> {
        StableMSBRadixSorter {
            delegate,
            fixed_start_offsets: vec![0; HISTOGRAM_SIZE],
            max_length,
        }
    }
}

impl<T> Sorter for StableMSBRadixSorter<T> where T: StableMSBRadixSorterBase {}

impl<T> MSBRadixSorterBase for StableMSBRadixSorter<T>
where
    T: StableMSBRadixSorterBase,
{
    fn byte_at(&mut self, i: i32, k: i32) -> Result<i32> {
        self.delegate.byte_at(i, k)
    }

    fn get_fallback_sorter(&mut self, k: i32, _length: i32) -> impl Sorter {
        let delegate = MergeSorterImpl::new(k as usize, self.max_length, &mut self.delegate);
        MergeSorter {
            delegate,
            pivot_index: 0,
        }
    }

    fn reorder(
        &mut self,
        from: i32,
        to: i32,
        start_offsets: &mut [i32],
        end_offsets: &mut [i32],
        k: i32,
    ) -> Result<()> {
        // Copy start_offsets to fixed_start_offsets
        self.fixed_start_offsets.copy_from(start_offsets, 0);

        for (i, &limit) in end_offsets.iter().enumerate().take(HISTOGRAM_SIZE) {
            let mut h1 = self.fixed_start_offsets[i];
            while h1 < limit {
                let b = self.get_bucket(from + h1, k)?;
                let h2 = start_offsets[b as usize];
                start_offsets[b as usize] += 1;
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
    fn save(&mut self, i: i32, j: i32);
    /// Restore values between i-th and j-th(excluding) in temporary storage
    /// into original storage.
    fn restore(&mut self, i: i32, j: i32);
}

pub struct MergeSorter<T>
where
    T: StableMSBRadixSorterBase,
{
    pub(crate) delegate: T,
    pub(crate) pivot_index: i32,
}

impl<T> MergeSorter<T>
where
    T: StableMSBRadixSorterBase,
{
    fn merge_sort(&mut self, from: i32, to: i32) -> Result<()> {
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
    fn bulk_save(&mut self, from: i32, tmp_from: i32, len: i32) {
        for i in 0..len {
            self.delegate.save(from + i, tmp_from + i);
        }
    }
    fn merge(&mut self, from: i32, to: i32, mid: i32) -> Result<()> {
        debug_assert!(
            to > mid && mid > from,
            "Invalid indices: to={to}, mid={mid}, from={from}"
        );
        // If already sorted, return early
        if self
            .delegate
            .compare((mid - 1) as usize, mid as usize)?
            <= 0
        {
            return Ok(());
        }
        let mut left = from;
        let mut right = mid;
        let mut index = from;
        loop {
            let cmp = self
                .delegate
                .compare(left as usize, right as usize)?;

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

    fn set_pivot(&mut self, i: i32) -> Result<()> {
        self.pivot_index = i;
        Ok(())
    }

    fn compare_pivot(&mut self, i: i32) -> Result<i32> {
        self.compare(self.pivot_index as usize, i as usize)
    }

    fn sort(&mut self, from: i32, to: i32) -> Result<()> {
        check_range(from, to)?;
        self.merge_sort(from, to)?;
        Ok(())
    }
}

pub struct MergeSorterImpl<'a, T>
where
    T: StableMSBRadixSorterBase,
{
    k: usize,
    max_length: usize,
    delegate: &'a mut T,
}
impl<'a, T> MergeSorterImpl<'a, T>
where
    T: StableMSBRadixSorterBase,
{
    pub fn new(k: usize, max_length: usize, delegate: &'a mut T) -> MergeSorterImpl<'a, T>
    where
        T: StableMSBRadixSorterBase,
    {
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
    fn compare(&mut self, i: usize, j: usize) -> Result<i32> {
        for o in self.k..self.max_length {
            let b1 = self.delegate.byte_at(i as i32, o.try_into()?)?;
            let b2 = self.delegate.byte_at(j as i32, o.try_into()?)?;
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
    fn save(&mut self, i: i32, j: i32) {
        self.delegate.save(i, j);
    }

    fn restore(&mut self, i: i32, j: i32) {
        self.delegate.restore(i, j);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use rand::Rng;

    use crate::core::index::{BytesRef, BytesRefBuilder};
    use crate::core::util::error::lucene_error::Result;
    use crate::core::util::stable_msb_radix_sorter::{
        StableMSBRadixSorter, StableMSBRadixSorterBase,
    };
    use crate::core::util::{MSBRadixSorter, MSBRadixSorterBase, SliceCopyOps, Sorter};
    use crate::test::util::common_method::assert_vecs_equal;
    use crate::test::util::lucene_test_case::lucene_test_case_util::{at_least, random};
    use crate::test::util::test_util::TestUtil;

    #[allow(dead_code)] // for quick search
    struct TestStableMSBRadixSorter;

    fn test<R: Rng + ?Sized>(refs: &[BytesRef<Vec<u8>>], len: usize, random: &mut R) -> Result<()> {
        let mut expected: Vec<BytesRef<Vec<u8>>> = refs[..len].to_vec();
        expected.sort();

        let mut max_length = 0;
        for ref_item in &refs[..len] {
            max_length = max_length.max(ref_item.length as i32);
        }

        match random.random_range(0..3) {
            0 => max_length += TestUtil::next_int(random, 1, 5),
            1 => max_length = i32::MAX,
            _ => {},
        }

        let final_max_length = max_length;
        let mut actual = refs[..len].to_vec();
        let delegate = StableMSBRadixSorterTestImpl::new(final_max_length, &mut actual);
        let stable_msb_radix_sorter =
            StableMSBRadixSorter::new(delegate, final_max_length.try_into()?);
        let mut msb_radix_sorter = MSBRadixSorter::new(max_length, stable_msb_radix_sorter);
        msb_radix_sorter.sort(0, len as i32)?;

        assert_vecs_equal(&expected, &actual);
        Ok(())
    }
    #[test]
    fn test_empty() -> Result<()> {
        let mut random = random();
        let refs: Vec<BytesRef<Vec<u8>>> = vec![BytesRef::default(); random.random_range(0..5)];
        test(&refs, 0, &mut random)
    }
    #[test]
    fn test_one_value() -> Result<()> {
        let mut random = random();
        let bytes = BytesRef::from_string(&TestUtil::random_simple_string(&mut random));
        let refs = vec![bytes];
        test(&refs, 1, &mut random)
    }

    #[test]
    fn test_two_values() -> Result<()> {
        let mut random = random();
        let bytes1 = BytesRef::from_string(&TestUtil::random_simple_string(&mut random));
        let bytes2 = BytesRef::from_string(&TestUtil::random_simple_string(&mut random));
        let refs = vec![bytes1, bytes2];
        test(&refs, 2, &mut random)
    }

    fn test_random_impl<R: Rng + ?Sized>(
        common_prefix_len: usize,
        max_len: usize,
        random: &mut R,
    ) -> Result<()> {
        let mut common_prefix = vec![0u8; common_prefix_len];
        random.fill_bytes(&mut common_prefix);
        let len = random.random_range(0..100_000);
        let mut bytes: Vec<BytesRef<Vec<u8>>> =
            Vec::with_capacity(len + random.random_range(0..50));
        for _ in 0..len {
            let mut b = vec![0u8; common_prefix_len + random.random_range(0..max_len)];
            random.fill_bytes(&mut b[common_prefix_len..]);
            b.copy_from(&common_prefix, 0);
            bytes.push(BytesRef::from_bytes(b));
        }
        test(&bytes, len, random)
    }

    #[test]
    fn test_random() -> Result<()> {
        let mut random = random();
        for _ in 0..10 {
            test_random_impl(0, 10, &mut random)?;
        }
        Ok(())
    }

    #[test]
    fn test_random_with_lots_of_duplicates() -> Result<()> {
        let mut random = random();
        for _ in 0..10 {
            test_random_impl(0, 2, &mut random)?;
        }
        Ok(())
    }

    #[test]
    fn test_random_with_shared_prefix() -> Result<()> {
        let mut random = random();
        for _ in 0..10 {
            let common_prefix_len = TestUtil::next_int(&mut random, 1, 30);
            test_random_impl(common_prefix_len as usize, 10, &mut random)?;
        }
        Ok(())
    }

    #[test]
    fn test_random_with_shared_prefix_and_lots_of_duplicates() -> Result<()> {
        let mut random = random();
        for _ in 0..10 {
            let common_prefix_len = TestUtil::next_int(&mut random, 1, 30);
            test_random_impl(common_prefix_len as usize, 2, &mut random)?;
        }
        Ok(())
    }

    #[test]
    fn test_random2() -> Result<()> {
        let mut random = random();
        // how large our alphabet is
        let letter_count = TestUtil::next_int(&mut random, 2, 10);

        // how many substring fragments to use
        let substring_count = TestUtil::next_int(&mut random, 2, 10) as usize;
        let mut substrings_set = HashSet::new();

        // how many strings to make
        let string_count = at_least(&mut random, 10000) as usize;

        // Generate substring fragments
        while substrings_set.len() < substring_count {
            let length = TestUtil::next_int(&mut random, 2, 10) as usize;
            let mut bytes = vec![0u8; length];
            for byte in &mut bytes {
                *byte = random.random_range(0..letter_count) as u8;
            }
            substrings_set.insert(BytesRef::from_bytes(bytes));
        }

        let substrings: Vec<BytesRef<Vec<u8>>> = substrings_set.into_iter().collect();
        let mut chance: Vec<f64> = Vec::with_capacity(substrings.len());
        let mut sum = 0.0;

        // Generate random chances
        for _ in &substrings {
            let value = random.random::<f64>();
            chance.push(value);
            sum += value;
        }

        // give each substring a random chance of occurring:
        let mut accum = 0.0;
        for value in &mut chance {
            accum += *value / sum;
            *value = accum;
        }

        let mut strings_set = HashSet::new();
        let mut iters = 0;

        while strings_set.len() < string_count && iters < string_count * 5 {
            let count = TestUtil::next_int(&mut random, 1, 5);
            let mut builder = BytesRefBuilder::new();

            for _ in 0..count {
                let v = random.random::<f64>();
                let mut accum = 0.0;
                for (j, substring) in substrings.iter().enumerate() {
                    accum += chance[j];
                    if accum >= v {
                        builder.append_ref(substring);
                        break;
                    }
                }
            }

            let br = builder.get_bytes_ref_copy();
            strings_set.insert(br);
            iters += 1;
        }

        let strings_vec: Vec<BytesRef<Vec<u8>>> = strings_set.into_iter().collect();
        test(&strings_vec, strings_vec.len(), &mut random)
    }

    struct StableMSBRadixSorterTestImpl<'a> {
        temp: Vec<BytesRef<Vec<u8>>>,
        final_max_length: i32,
        refs: &'a mut [BytesRef<Vec<u8>>],
    }
    impl<'a> StableMSBRadixSorterTestImpl<'a> {
        fn new(final_max_length: i32, refs: &'a mut Vec<BytesRef<Vec<u8>>>) -> Self {
            StableMSBRadixSorterTestImpl {
                temp: vec![BytesRef::default(); refs.len()],
                final_max_length,
                refs,
            }
        }
    }

    impl Sorter for StableMSBRadixSorterTestImpl<'_> {
        fn swap(&mut self, i: usize, j: usize) -> Result<()> {
            self.refs.swap(i, j);
            Ok(())
        }
    }

    impl MSBRadixSorterBase for StableMSBRadixSorterTestImpl<'_> {
        fn byte_at(&mut self, i: i32, k: i32) -> Result<i32> {
            assert!(k < self.final_max_length, "k is out of bounds");
            let ref_item = &self.refs[i as usize];

            if ref_item.length as i32 <= k {
                return Ok(-1);
            }

            Ok(ref_item.bytes[ref_item.offset + k as usize] as i32)
        }
    }
    impl StableMSBRadixSorterBase for StableMSBRadixSorterTestImpl<'_> {
        fn save(&mut self, i: i32, j: i32) {
            self.temp[j as usize] = self.refs[i as usize].clone();
        }

        fn restore(&mut self, i: i32, j: i32) {
            for idx in i..j {
                self.refs[idx as usize] = self.temp[idx as usize].clone();
            }
        }
    }
}
