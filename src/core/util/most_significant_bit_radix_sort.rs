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
use crate::core::util::intro_sorter::IntroSorter;
use crate::core::util::{Sorter, check_range};

/// After this many levels of recursion, we fall back to introsort.
/// This protects against poor performance when there are long common prefixes,
/// likely due to cache locality issues.
pub(crate) const LEVEL_THRESHOLD: usize = 8;
/// Size of histograms: 256 + 1 to indicate that the string is finished.
pub(crate) const HISTOGRAM_SIZE: usize = 257;
/// Buckets below this size will be sorted with the fallback sorter.
pub(crate) const LENGTH_THRESHOLD: usize = 100;
pub struct MSBRadixSorter<T>
where
    T: MSBRadixSorterBase,
{
    /// One histogram per recursion level.
    histograms: Vec<Vec<i32>>,
    /// End offsets for histograms.
    end_offsets: Vec<i32>,
    /// Array to store common prefixes.
    common_prefix: Vec<i32>,
    /// Maximum length of strings to sort.
    max_length: i32,
    delegate: T,
}
impl<T> MSBRadixSorter<T>
where
    T: MSBRadixSorterBase,
{
    /// Sole constructor.
    ///
    /// # Parameters
    /// - `max_length`: The maximum length of keys. Pass `i32::MAX` if unknown.
    pub fn new(max_length: i32, delegate: T) -> Self {
        let histograms: Vec<Vec<i32>> = (0..LEVEL_THRESHOLD).map(|_| Vec::new()).collect();
        Self {
            histograms,
            end_offsets: vec![0; HISTOGRAM_SIZE],
            max_length,
            common_prefix: vec![0; 24.min(max_length as usize)],
            delegate,
        }
    }
    pub fn sort_impl(&mut self, from: i32, to: i32, k: i32, l: i32) -> Result<()> {
        if self.should_fallback(from, to, l) {
            self.get_fallback_sorter(k).sort(from, to)
        } else {
            self.radix_sort(from, to, k, l)
        }
    }
    fn should_fallback(&self, from: i32, to: i32, l: i32) -> bool {
        self.delegate.should_fallback(from, to, l)
    }
    /// Computes the initial common prefix length for the given range.
    ///
    /// This method has been split to avoid platform-specific issues.
    fn compute_initial_common_prefix_length(&mut self, from: i32, k: i32) -> Result<i32> {
        let common_prefix = &mut self.common_prefix;
        let mut common_prefix_length =
            std::cmp::min(common_prefix.len(), (self.max_length - k) as usize);

        for (j, slot) in common_prefix
            .iter_mut()
            .enumerate()
            .take(common_prefix_length)
        {
            let b = self.delegate.byte_at(from, k + j as i32)?;
            *slot = b;
            if b == -1 {
                common_prefix_length = j + 1;
                break;
            }
        }
        Ok(common_prefix_length as i32)
    }
    fn compute_common_prefix_length_and_build_histogram_part2(
        &mut self,
        from: i32,
        to: i32,
        k: i32,
        l: i32,
        common_prefix_length: i32,
        i: i32,
    ) -> Result<i32> {
        if i < to {
            debug_assert!(common_prefix_length == 0);
            self.build_histogram(self.common_prefix[0] + 1, i - from, i, to, k, l)?;
        } else {
            debug_assert!(common_prefix_length > 0);
            self.histograms[l as usize][(self.common_prefix[0] + 1) as usize] = to - from;
        }

        Ok(common_prefix_length)
    }
    /// Build a histogram of the k-th characters of values occurring between
    /// offsets `from` and `to`, using the `get_bucket` method.
    fn build_histogram(
        &mut self,
        prefix_common_bucket: i32,
        prefix_common_len: i32,
        from: i32,
        to: i32,
        k: i32,
        l: i32,
    ) -> Result<()> {
        self.delegate.build_histogram(
            prefix_common_bucket,
            prefix_common_len,
            from,
            to,
            k,
            &mut self.histograms[l as usize],
        )
    }
    fn compute_common_prefix_length_and_build_histogram_part1(
        &mut self,
        from: i32,
        to: i32,
        k: i32,
        l: i32,
        mut common_prefix_length: i32,
    ) -> Result<i32> {
        let mut i = from + 1;

        'outer: for idx in from + 1..to {
            let mut j = 0;
            while j < common_prefix_length {
                let b = self.delegate.byte_at(idx, k + j)?;
                if b != self.common_prefix[j as usize] {
                    common_prefix_length = j;
                    if common_prefix_length == 0 {
                        break 'outer;
                    }
                    break;
                }
                j += 1;
            }
            i = idx + 1;
        }

        self.compute_common_prefix_length_and_build_histogram_part2(
            from,
            to,
            k,
            l,
            common_prefix_length,
            i,
        )
    }
    pub fn compute_common_prefix_length_and_build_histogram(
        &mut self,
        from: i32,
        to: i32,
        k: i32,
        l: i32,
    ) -> Result<i32> {
        let common_prefix_length = self.compute_initial_common_prefix_length(from, k)?;
        self.compute_common_prefix_length_and_build_histogram_part1(
            from,
            to,
            k,
            l,
            common_prefix_length,
        )
    }
    fn sum_histogram(histogram: &mut [i32], end_offsets: &mut [i32]) {
        let mut accum = 0;
        for (hist, end_offset) in histogram.iter_mut().zip(end_offsets.iter_mut()) {
            let count = *hist;
            *hist = accum;
            accum += count;
            *end_offset = accum;
        }
    }
    /// Reorder based on start/end offsets for each bucket. When this method
    /// returns, `start_offsets` and `end_offsets` are equal.
    ///
    /// # Parameters
    /// - `from`: The starting index (inclusive).
    /// - `to`: The ending index (exclusive).
    /// - `start_offsets`: Start offsets per bucket.
    /// - `end_offsets`: End offsets per bucket.
    /// - `k`: The current position offset.
    fn reorder(&mut self, from: i32, to: i32, l: i32, k: i32) -> Result<()> {
        self.delegate.reorder(
            from,
            to,
            &mut self.histograms[l as usize],
            &mut self.end_offsets,
            k,
        )
    }
    /// Performs radix sort on the specified range and recursion level.
    ///
    /// # Parameters
    /// - `from`: Start index (inclusive).
    /// - `to`: End index (exclusive).
    /// - `k`: The character number to compare.
    /// - `l`: The level of recursion.
    fn radix_sort(&mut self, from: i32, to: i32, k: i32, l: i32) -> Result<()> {
        // Access or initialize the histogram for this level
        if self.histograms[l as usize].is_empty() {
            self.histograms[l as usize] = vec![0; HISTOGRAM_SIZE];
        } else {
            self.histograms[l as usize].fill(0);
        }

        // Compute the common prefix length and build the histogram
        let common_prefix_length =
            self.compute_common_prefix_length_and_build_histogram(from, to, k, l)?;

        if common_prefix_length > 0 {
            // if there are no more chars to compare or if all entries fell into
            // the first bucket (which means strings are shorter
            // than k) then we are done otherwise recurse
            if k + common_prefix_length < self.max_length
                && self.histograms[l as usize][0] < (to - from)
            {
                self.radix_sort(from, to, k + common_prefix_length, l)?;
            }
            return Ok(());
        }

        // Assert histogram correctness (can be implemented as a debug check)
        debug_assert!(Self::assert_histogram(
            common_prefix_length,
            &self.histograms[l as usize]
        ));

        // Prepare start and end offsets
        Self::sum_histogram(&mut self.histograms[l as usize], &mut self.end_offsets);

        // Reorder the range
        self.reorder(from, to, l, k)?;

        // Update end offsets

        // Recursively sort buckets if more levels are allowed
        if k + 1 < self.max_length {
            let mut prev = self.histograms[l as usize][0];
            for i in 1..HISTOGRAM_SIZE {
                let h = self.histograms[l as usize][i];
                let bucket_len = h - prev;
                if bucket_len > 1 {
                    self.sort_impl(from + prev, from + h, k + 1, l + 1)?;
                }
                prev = h;
            }
        }
        Ok(())
    }

    fn get_fallback_sorter(&mut self, k: i32) -> impl Sorter + use<'_, T> {
        self.delegate.get_fallback_sorter(k, self.max_length)
    }

    /// Always returns `true` if the assertions pass.
    fn assert_histogram(common_prefix_length: i32, histogram: &[i32]) -> bool {
        let number_of_unique_bytes = histogram.iter().filter(|&&freq| freq > 0).count();

        if number_of_unique_bytes == 1 {
            debug_assert!(common_prefix_length >= 1);
        } else {
            debug_assert!(
                common_prefix_length == 0,
                "Expected common_prefix_length to be 0, but found {common_prefix_length}"
            );
        }
        true
    }
    #[cfg(debug_assertions)]
    pub fn get_delegate(&self) -> &T {
        &self.delegate
    }
}

impl<T> Sorter for MSBRadixSorter<T>
where
    T: MSBRadixSorterBase,
{
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
        check_range(from, to)?;
        self.sort_impl(from, to, 0, 0)
    }
}

pub struct MSBRadixIntroSorterImpl<'a, T>
where
    T: MSBRadixSorterBase,
{
    pivot: BytesRefBuilder<Vec<u8>>,
    max_length: i32,
    k: i32,
    delegate: &'a mut T,
}

impl<T> Sorter for MSBRadixIntroSorterImpl<'_, T>
where
    T: MSBRadixSorterBase,
{
    fn compare(&mut self, i: usize, j: usize) -> Result<i32> {
        for o in self.k..self.max_length {
            let b1 = self.delegate.byte_at(i as i32, o)?;
            let b2 = self.delegate.byte_at(j as i32, o)?;

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

    fn set_pivot(&mut self, i: i32) -> Result<()> {
        self.pivot.set_length(0);

        for o in self.k..self.max_length {
            let b = self.delegate.byte_at(i, o)?;
            if b == -1 {
                break;
            }
            self.pivot.append_byte(b as u8);
        }
        Ok(())
    }

    fn compare_pivot(&mut self, j: i32) -> Result<i32> {
        for o in 0..self.pivot.length() {
            let b1 = self.pivot.byte_at(o) as i32;
            let b2 = self.delegate.byte_at(j, self.k + o as i32)?;
            if b1 != b2 {
                return Ok(b1 - b2);
            }
        }

        if self.k + self.pivot.length() as i32 == self.max_length {
            Ok(0)
        } else {
            Ok(-1
                - self
                    .delegate
                    .byte_at(j, self.k + self.pivot.length() as i32)?)
        }
    }

    fn sort(&mut self, from: i32, to: i32) -> Result<()> {
        IntroSorter::sort_range(self, from, to)?;
        Ok(())
    }
}

impl<T> IntroSorter for MSBRadixIntroSorterImpl<'_, T> where T: MSBRadixSorterBase {}

pub trait MSBRadixSorterBase: Sorter {
    /// Returns the k-th byte of the entry at the given index `i`, or `-1` if
    /// its length is less than or equal to `k`.
    ///
    /// # Parameters
    /// - `i`: The index of the entry, which must be between `0` (inclusive) and
    ///   `max_length` (exclusive).
    /// - `k`: The position of the byte to retrieve within the entry.
    ///
    /// # Returns
    /// The k-th byte of the entry at index `i` as an `i32`, or `-1` if the
    /// entry's length is less than or equal to `k`.
    ///
    /// # Note
    /// In Rust, this method might return a signed integer (`i32`) to
    /// accommodate the `-1` case, which differs from Java's default integer
    /// handling.
    fn byte_at(&mut self, _i: i32, _k: i32) -> Result<i32> {
        Err(LuceneError::not_implemented(""))
    }

    fn get_fallback_sorter(&mut self, k: i32, length: i32) -> impl Sorter
    where
        Self: Sized,
    {
        MSBRadixIntroSorterImpl {
            pivot: BytesRefBuilder::new(),
            max_length: length,
            k,
            delegate: self,
        }
    }

    /// Reorder based on start/end offsets for each bucket. When this method
    /// returns, `start_offsets` and `end_offsets` are equal.
    ///
    /// # Parameters
    /// - `from`: The starting index (inclusive).
    /// - `to`: The ending index (exclusive).
    /// - `start_offsets`: Start offsets per bucket.
    /// - `end_offsets`: End offsets per bucket.
    /// - `k`: The current position offset.
    fn reorder(
        &mut self,
        from: i32,
        _to: i32,
        start_offsets: &mut [i32],
        end_offsets: &mut [i32],
        k: i32,
    ) -> Result<()> {
        // Reorder in place, similar to the Dutch national flag problem
        for i in 0..HISTOGRAM_SIZE {
            let limit = end_offsets[i];
            while start_offsets[i] < limit {
                let h1 = start_offsets[i];
                let b = self.get_bucket(from + h1, k)?;
                let h2 = start_offsets[b as usize];
                start_offsets[b as usize] += 1;
                self.swap((from + h1) as usize, (from + h2) as usize)?;
            }
        }
        Ok(())
    }

    fn get_bucket(&mut self, i: i32, k: i32) -> Result<i32> {
        Ok(self.byte_at(i, k)? + 1)
    }

    fn build_histogram(
        &mut self,
        prefix_common_bucket: i32,
        prefix_common_len: i32,
        from: i32,
        to: i32,
        k: i32,
        histogram: &mut [i32],
    ) -> Result<()> {
        histogram[prefix_common_bucket as usize] = prefix_common_len;

        for i in from..to {
            let b = self.get_bucket(i, k)? as usize;
            histogram[b] += 1;
        }
        Ok(())
    }

    fn should_fallback(&self, from: i32, to: i32, l: i32) -> bool {
        (to - from) <= LENGTH_THRESHOLD as i32 || l >= LEVEL_THRESHOLD as i32
    }
}
#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashSet};

    use rand::Rng;

    use crate::core::index::{BytesRef, BytesRefBuilder};
    use crate::core::util::error::lucene_error::Result;
    use crate::core::util::{MSBRadixSorter, MSBRadixSorterBase, SliceCopyOps, Sorter};
    use crate::test::util::common_method::assert_vecs_equal;
    use crate::test::util::lucene_test_case::lucene_test_case_util::{at_least, random};
    use crate::test::util::test_util::TestUtil;

    #[allow(dead_code)] // for quick search
    struct TestMSBRadixSorter;

    fn test<R: Rng + ?Sized>(
        refs: &mut [BytesRef<Vec<u8>>],
        len: usize,
        random: &mut R,
    ) -> Result<()> {
        let mut expected: Vec<BytesRef<Vec<u8>>> = refs[..len].to_vec();
        expected.sort();

        let mut max_length: i32 = 0;
        for ref_item in &refs[..len] {
            max_length = max_length.max(ref_item.length as i32);
        }

        match random.random_range(0..3) {
            0 => max_length += TestUtil::next_int(random, 1, 5),
            1 => max_length = i32::MAX,
            _ => {},
        }

        let final_max_length = max_length;
        let delegate = MSBRadixSorterImpl::new(final_max_length, refs[..len].to_vec());
        let mut msb_radix_sorter = MSBRadixSorter::new(max_length, delegate);
        msb_radix_sorter.sort(0, len as i32)?;

        assert_vecs_equal(&expected, &msb_radix_sorter.get_delegate().refs);
        Ok(())
    }
    #[test]
    fn test_empty() -> Result<()> {
        let mut random = random();
        let mut refs: Vec<BytesRef<Vec<u8>>> = vec![BytesRef::default(); random.random_range(0..5)];
        assert!(test(&mut refs, 0, &mut random).is_ok());
        test(&mut refs, 0, &mut random)
    }
    #[test]
    fn test_one_value() -> Result<()> {
        let mut random = random();

        let bytes = BytesRef::from_string(&TestUtil::random_simple_string(&mut random));
        let mut refs = vec![bytes];
        test(&mut refs, 1, &mut random)
    }
    #[test]
    fn test_two_values() -> Result<()> {
        let mut random = random();

        let bytes1 = BytesRef::from_string(&TestUtil::random_simple_string(&mut random));
        let bytes2 = BytesRef::from_string(&TestUtil::random_simple_string(&mut random));
        let mut refs = vec![bytes1, bytes2];

        test(&mut refs, 2, &mut random)
    }

    fn test_random_impl<R: Rng + ?Sized>(
        common_prefix_len: usize,
        max_len: i32,
        random: &mut R,
    ) -> Result<()> {
        let mut common_prefix = vec![0u8; common_prefix_len];
        random.fill_bytes(&mut common_prefix);
        let len = random.random_range(0..10000);
        let mut bytes: Vec<BytesRef<Vec<u8>>> =
            Vec::with_capacity(len + random.random_range(0..50));
        for _ in 0..len {
            let mut b = vec![0u8; common_prefix_len + random.random_range(0..max_len) as usize];
            random.fill_bytes(&mut b[common_prefix_len..]);

            b.copy_from(&common_prefix, 0);

            bytes.push(BytesRef::from_bytes(b));
        }
        test(&mut bytes, len, random)
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
            let shared_prefix = TestUtil::next_int(&mut random, 1, 30) as usize;
            test_random_impl(shared_prefix, 10, &mut random)?;
        }
        Ok(())
    }

    #[test]
    fn test_random_with_shared_prefix_and_lots_of_duplicates() -> Result<()> {
        let mut random = random();
        for _ in 0..10 {
            let shared_prefix = TestUtil::next_int(&mut random, 1, 30) as usize;
            test_random_impl(shared_prefix, 2, &mut random)?;
        }
        Ok(())
    }

    #[test]
    fn test_random2() -> Result<()> {
        let mut random = random();
        // How large our alphabet is
        let letter_count = TestUtil::next_int(&mut random, 2, 10);

        // How many substring fragments to use
        let substring_count = TestUtil::next_int(&mut random, 2, 10) as usize;
        let mut substrings_set = HashSet::new();

        // How many strings to make
        let string_count = at_least(&mut random, 10000) as usize;
        // let string_count = ;

        // Generate unique substrings
        while substrings_set.len() < substring_count {
            let length = TestUtil::next_int(&mut random, 2, 10);
            let bytes: Vec<u8> = (0..length)
                .map(|_| random.random_range(0..letter_count) as u8)
                .collect();
            let br = BytesRef::from_bytes(bytes);
            substrings_set.insert(br);
        }

        let substrings: Vec<BytesRef<Vec<u8>>> = Vec::from_iter(substrings_set);
        let mut chance = vec![0.0; substrings.len()];
        let mut sum = 0.0;

        for chance_value in &mut chance {
            *chance_value = random.random::<f64>();
            sum += *chance_value;
        }

        // give each substring a random chance of occurring:
        let mut accum = 0.0;
        for chance_value in chance.iter_mut() {
            accum += *chance_value / sum;
            *chance_value = accum;
        }

        // Generate unique strings
        let mut strings_set = BTreeSet::new();
        let mut iters = 0;
        while strings_set.len() < string_count && iters < string_count * 5 {
            let count = random.random_range(1..=5);
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

        // Run test with generated strings
        let strings: Vec<BytesRef<Vec<u8>>> = strings_set.into_iter().collect();
        test(&mut strings.clone(), strings.len(), &mut random)
    }

    pub struct MSBRadixSorterImpl {
        final_max_length: i32,
        refs: Vec<BytesRef<Vec<u8>>>,
    }

    impl MSBRadixSorterImpl {
        fn new(final_max_length: i32, refs: Vec<BytesRef<Vec<u8>>>) -> Self {
            Self {
                final_max_length,
                refs,
            }
        }
    }

    impl MSBRadixSorterBase for MSBRadixSorterImpl {
        fn byte_at(&mut self, i: i32, k: i32) -> Result<i32> {
            assert!(
                k < self.final_max_length,
                "Index out of bounds: k={} exceeds final_max_length={}",
                k,
                self.final_max_length
            );

            let ref_item = &self.refs[i as usize];
            if ref_item.length <= k as usize {
                Ok(-1)
            } else {
                Ok(ref_item.bytes[ref_item.offset + k as usize] as i32)
            }
        }
    }
    impl Sorter for MSBRadixSorterImpl {
        fn swap(&mut self, i: usize, j: usize) -> Result<()> {
            self.refs.swap(i, j);
            Ok(())
        }
    }
}
