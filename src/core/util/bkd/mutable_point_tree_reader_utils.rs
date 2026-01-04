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
use crate::core::codecs::mutable_point_tree::MutablePointTree;
use crate::core::index::BytesRef;
use crate::core::util::array_util::{ArrayUtil, ByteArrayComparator, ByteArrayComparatorEnum};
use crate::core::util::bkd::bkd_config::BKDConfig;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::intro_sorter::IntroSorter;
use crate::core::util::packed::PackedInts;
use crate::core::util::radix_selector::{RadixSelector, RadixSelectorBase};
use crate::core::util::selector::Selector;
use crate::core::util::{
    IntroSelector, IntroSelectorBase, IntroSelectorBaseDefault, MSBRadixSorter, MSBRadixSorterBase,
    Sorter, StableMSBRadixSorter, StableMSBRadixSorterBase, ToInt,
};

/// Utility APIs for sorting and partitioning buffered points.
pub struct MutablePointTreeReaderUtils;

impl MutablePointTreeReaderUtils {
    /// Sort the given [`MutablePointTree`] based on its packed value then doc
    /// ID.
    pub fn sort<M>(
        config: &BKDConfig,
        max_doc: i32,
        reader: &mut M,
        from: usize,
        to: usize,
    ) -> Result<()>
    where
        M: MutablePointTree,
    {
        let mut sorted_by_doc_id = true;
        let mut prev_doc = 0;
        for i in from..to {
            let doc = reader.get_doc_id(i);
            if doc < prev_doc {
                sorted_by_doc_id = false;
                break;
            }
            prev_doc = doc;
        }

        // No need to tie break on doc IDs if already sorted by doc ID, since we
        // use a stable sort. This should be a common situation as
        // IndexWriter accumulates data in doc ID order when
        // index sorting is not enabled.
        let bits_per_doc_id: usize = if sorted_by_doc_id {
            0
        } else {
            PackedInts::bits_required((max_doc - 1) as i64)?.try_into()?
        };
        let max_length = config.packed_bytes_length() + bits_per_doc_id.div_ceil(8);
        let delegate = StableMSBRadixSorterImpl {
            reader,
            config,
            bits_per_doc_id,
        };
        let stable_msb_radix_sorter = StableMSBRadixSorter::new(delegate, max_length);
        let mut sorter = MSBRadixSorter::new(max_length.try_into()?, stable_msb_radix_sorter);
        sorter.sort(from.try_into()?, to.try_into()?)
    }

    /// Sort points on the given dimension.
    #[allow(clippy::too_many_arguments)]
    pub fn sort_by_dim<M>(
        config: &BKDConfig,
        sorted_dim: usize,
        _common_prefix_lengths: &[usize],
        reader: &mut M,
        from: usize,
        to: usize,
        _scratch1: &mut BytesRef<Vec<u8>>,
        _scratch2: &mut BytesRef<Vec<u8>>,
    ) -> Result<()>
    where
        M: MutablePointTree,
    {
        // Get an unsigned comparator for the byte arrays.
        let comparator = ArrayUtil::get_unsigned_comparator(config.bytes_per_dim);
        let start = sorted_dim * config.bytes_per_dim;
        // No need for a fancy radix sort here, this is called on the leaves
        // only so there are not many values to sort.
        let mut intro_sorter = IntroSorterImpl {
            reader,
            config,
            pivot: BytesRef::new(),
            scratch2: BytesRef::new(),
            pivot_doc: 0,
            comparator,
            start,
        };
        intro_sorter.sort(from.try_into()?, to.try_into()?)?;
        Ok(())
    }
    /// Partition points around `mid`. All values on the left must be less than
    /// or equal to it and all values on the right must be greater than or
    /// equal to it.
    #[allow(clippy::too_many_arguments)]
    pub fn partition<M>(
        config: &BKDConfig,
        max_doc: i32,
        split_dim: usize,
        common_prefix_len: usize,
        reader: &mut M,
        from: usize,
        to: usize,
        mid: usize,
        _scratch1: &mut BytesRef<Vec<u8>>,
        _scratch2: &mut BytesRef<Vec<u8>>,
    ) -> Result<()>
    where
        M: MutablePointTree,
    {
        let dim_offset = split_dim * config.bytes_per_dim + common_prefix_len;
        let dim_cmp_bytes = config.bytes_per_dim - common_prefix_len;
        debug_assert!(config.num_dims >= config.num_index_dims);
        let data_cmp_bytes =
            (config.num_dims - config.num_index_dims) * config.bytes_per_dim + dim_cmp_bytes;
        let bits_per_doc_id = PackedInts::bits_required((max_doc - 1) as i64)? as usize;
        let max_length = data_cmp_bytes + bits_per_doc_id.div_ceil(8);

        let sub_selector = RadixSelectorImpl {
            split_dim,
            config,
            dim_cmp_bytes,
            reader,
            dim_offset,
            data_cmp_bytes,
            bits_per_doc_id,
        };
        let mut radix_selector = RadixSelector::new(max_length.try_into()?, sub_selector);
        radix_selector.select(from.try_into()?, to.try_into()?, mid.try_into()?)
    }
}

struct StableMSBRadixSorterImpl<'a, M>
where
    M: MutablePointTree,
{
    reader: &'a mut M,
    config: &'a BKDConfig,
    bits_per_doc_id: usize,
}

impl<M> MSBRadixSorterBase for StableMSBRadixSorterImpl<'_, M>
where
    M: MutablePointTree,
{
    fn byte_at(&mut self, i: i32, k: i32) -> Result<i32> {
        if (k as usize) < self.config.packed_bytes_length() {
            Ok(self.reader.get_byte_at(i as usize, k as usize) as i32)
        } else {
            let rhs = (k - self.config.packed_bytes_length() as i32 + 1) << 3;

            let shift = if rhs >= 0 {
                self.bits_per_doc_id.checked_sub(rhs as usize)
            } else {
                self.bits_per_doc_id.checked_add((-rhs) as usize)
            };
            let effective_shift = match shift {
                Some(s) => s as u32,
                None => 0,
            };
            Ok(((self.reader.get_doc_id(i as usize) as u32 >> effective_shift) & 0xff) as i32)
        }
    }
}

impl<M> Sorter for StableMSBRadixSorterImpl<'_, M>
where
    M: MutablePointTree,
{
    fn swap(&mut self, i: usize, j: usize) -> Result<()> {
        self.reader.swap(i, j);
        Ok(())
    }
}

impl<M> StableMSBRadixSorterBase for StableMSBRadixSorterImpl<'_, M>
where
    M: MutablePointTree,
{
    fn save(&mut self, i: i32, j: i32) {
        self.reader.save(i as usize, j as usize);
    }

    fn restore(&mut self, i: i32, j: i32) {
        self.reader.restore(i as usize, j as usize);
    }
}

struct IntroSorterImpl<'a, M>
where
    M: MutablePointTree,
{
    reader: &'a mut M,
    config: &'a BKDConfig,
    pivot: BytesRef<Vec<u8>>,
    scratch2: BytesRef<Vec<u8>>,
    pivot_doc: i32,
    comparator: ByteArrayComparatorEnum,
    start: usize,
}
impl<M> Sorter for IntroSorterImpl<'_, M>
where
    M: MutablePointTree,
{
    fn compare(&mut self, i: usize, j: usize) -> Result<i32> {
        self.set_pivot(i as i32)?;
        self.compare_pivot(j as i32)
    }

    fn swap(&mut self, i: usize, j: usize) -> Result<()> {
        self.reader.swap(i, j);
        Ok(())
    }

    fn set_pivot(&mut self, i: i32) -> Result<()> {
        self.reader.get_value(i as usize, &mut self.pivot);
        self.pivot_doc = self.reader.get_doc_id(i as usize);
        Ok(())
    }

    fn compare_pivot(&mut self, j: i32) -> Result<i32> {
        self.reader.get_value(j as usize, &mut self.scratch2);

        let cmp = self.comparator.compare(
            &self.pivot.bytes,
            self.pivot.offset + self.start,
            &self.scratch2.bytes,
            self.scratch2.offset + self.start,
        );

        if cmp == 0 {
            let pivot_index_start = self.pivot.offset + self.config.packed_index_bytes_length();
            let pivot_index_end = self.pivot.offset + self.config.packed_bytes_length();
            let scratch_index_start =
                self.scratch2.offset + self.config.packed_index_bytes_length();
            let scratch_index_end = self.scratch2.offset + self.config.packed_bytes_length();

            let pivot_slice = &self.pivot.bytes[pivot_index_start..pivot_index_end];
            let scratch_slice = &self.scratch2.bytes[scratch_index_start..scratch_index_end];

            let cmp = pivot_slice.cmp(scratch_slice).to_int();
            return if cmp == 0 {
                Ok(self.pivot_doc - self.reader.get_doc_id(j as usize))
            } else {
                Ok(cmp)
            };
        }
        Ok(cmp)
    }

    fn sort(&mut self, from: i32, to: i32) -> Result<()> {
        IntroSorter::sort_range(self, from, to)?;
        Ok(())
    }
}

impl<M> IntroSorter for IntroSorterImpl<'_, M> where M: MutablePointTree {}

struct RadixSelectorImpl<'a, M>
where
    M: MutablePointTree,
{
    split_dim: usize,
    config: &'a BKDConfig,
    dim_cmp_bytes: usize,
    reader: &'a mut M,
    dim_offset: usize,
    data_cmp_bytes: usize,
    bits_per_doc_id: usize,
}

impl<M> Selector for RadixSelectorImpl<'_, M>
where
    M: MutablePointTree,
{
    fn swap(&mut self, i: i32, j: i32) -> Result<()> {
        self.reader.swap(i as usize, j as usize);
        Ok(())
    }
}

impl<M> RadixSelectorBase for RadixSelectorImpl<'_, M>
where
    M: MutablePointTree,
{
    fn byte_at(&mut self, i: i32, k: i32) -> Result<i32> {
        let k = k as usize;
        if k < self.dim_cmp_bytes {
            Ok(self.reader.get_byte_at(i as usize, self.dim_offset + k) as i32)
        } else if k < self.data_cmp_bytes {
            Ok(self.reader.get_byte_at(
                i as usize,
                self.config.packed_index_bytes_length() + k - self.dim_cmp_bytes,
            ) as i32)
        } else {
            let rhs: i32 = (k as i32 - self.data_cmp_bytes as i32 + 1) << 3;

            let shift = if rhs >= 0 {
                self.bits_per_doc_id.checked_sub(rhs as usize)
            } else {
                self.bits_per_doc_id.checked_add((-rhs) as usize)
            };
            let effective_shift = match shift {
                Some(s) => s as u32,
                None => 0,
            };
            Ok(((self.reader.get_doc_id(i as usize) as u32 >> effective_shift) & 0xff) as i32)
        }
    }

    fn get_fallback_selector(&mut self, k: i32, _max_length: i32) -> impl Selector
    where
        Self: Sized,
    {
        let k = k as usize;
        let dim_start = self.split_dim * self.config.bytes_per_dim;
        let data_start = if k < self.dim_cmp_bytes {
            self.config.packed_index_bytes_length()
        } else {
            self.config.packed_index_bytes_length() + k - self.dim_cmp_bytes
        };
        let data_end = self.config.num_dims * self.config.bytes_per_dim;
        let dim_comparator = ArrayUtil::get_unsigned_comparator(self.config.bytes_per_dim);

        let sub_selector = IntroSelectorImpl {
            dim_cmp_bytes: self.dim_cmp_bytes,
            data_cmp_bytes: self.data_cmp_bytes,
            pivot: BytesRef::new(),
            reader: self.reader,
            pivot_doc: 0,
            k: k as i32,
            scratch2: BytesRef::new(),
            dim_comparator,
            dim_start,
            data_start,
            data_end,
        };
        IntroSelector::new(sub_selector)
    }
}

struct IntroSelectorImpl<'a, M>
where
    M: MutablePointTree,
{
    dim_cmp_bytes: usize,
    data_cmp_bytes: usize,
    pivot: BytesRef<Vec<u8>>,
    reader: &'a mut M,
    pivot_doc: i32,
    k: i32,
    scratch2: BytesRef<Vec<u8>>,
    dim_comparator: ByteArrayComparatorEnum,
    dim_start: usize,
    data_start: usize,
    data_end: usize,
}

impl<M> IntroSelectorBaseDefault for IntroSelectorImpl<'_, M>
where
    M: MutablePointTree,
{
    fn set_pivot(&mut self, i: i32) -> Result<()> {
        self.reader.get_value(i as usize, &mut self.pivot);
        self.pivot_doc = self.reader.get_doc_id(i as usize);
        Ok(())
    }

    fn compare_pivot(&mut self, j: i32) -> Result<i32> {
        let k = self.k as usize;
        if k < self.dim_cmp_bytes {
            self.reader.get_value(j as usize, &mut self.scratch2);
            let cmp = self.dim_comparator.compare(
                &self.pivot.bytes,
                self.pivot.offset + self.dim_start,
                &self.scratch2.bytes,
                self.scratch2.offset + self.dim_start,
            );
            if cmp != 0 {
                return Ok(cmp);
            }
        }
        if k < self.data_cmp_bytes {
            self.reader.get_value(j as usize, &mut self.scratch2);
            let pivot_slice = &self.pivot.bytes
                [self.pivot.offset + self.data_start..self.pivot.offset + self.data_end];
            let scratch_slice = &self.scratch2.bytes
                [self.scratch2.offset + self.data_start..self.scratch2.offset + self.data_end];
            let cmp = pivot_slice.cmp(scratch_slice).to_int();
            if cmp != 0 {
                return Ok(cmp);
            }
        }
        Ok(self.pivot_doc - self.reader.get_doc_id(j as usize))
    }
}

impl<M> Selector for IntroSelectorImpl<'_, M>
where
    M: MutablePointTree,
{
    fn swap(&mut self, i: i32, j: i32) -> Result<()> {
        self.reader.swap(i as usize, j as usize);
        Ok(())
    }
}

impl<M> IntroSelectorBase for IntroSelectorImpl<'_, M> where M: MutablePointTree {}

#[cfg(test)]
pub(crate) mod tests {

    use std::fmt;
    use std::rc::Rc;

    use rand::Rng;

    use crate::core::codecs::mutable_point_tree::MutablePointTree;
    use crate::core::index::BytesRef;
    use crate::core::index::point_values::PointTree;
    use crate::core::util::bkd::bkd_config::BKDConfig;
    use crate::core::util::bkd::mutable_point_tree_reader_utils::MutablePointTreeReaderUtils;
    use crate::core::util::clone::TryClone;
    use crate::core::util::error::lucene_error::Result;
    use crate::core::util::{SliceCopyOps, ToInt};
    use crate::test::util::lucene_test_case::lucene_test_case_util::random;
    use crate::test::util::test_util::TestUtil;

    #[allow(dead_code)] // for quick search
    struct TestMutablePointTreeReaderUtils;
    #[test]
    fn test_sort() -> Result<()> {
        let mut random = random();
        for _ in 0..10 {
            do_test_sort(&mut random, false)?;
        }
        Ok(())
    }

    #[test]
    fn test_sort_with_incremental_doc_id() -> Result<()> {
        let mut random = random();
        for _ in 0..10 {
            do_test_sort(&mut random, true)?;
        }
        Ok(())
    }

    fn do_test_sort<R: Rng + ?Sized>(random: &mut R, is_doc_id_incremental: bool) -> Result<()> {
        let bytes_per_dim = TestUtil::next_int(random, 1, 16) as usize;
        let end = 1 << random.random_range(0..30);
        let max_doc = TestUtil::next_int(random, 1, end);
        let config = BKDConfig::new(
            1,
            1,
            bytes_per_dim,
            BKDConfig::DEFAULT_MAX_POINTS_IN_LEAF_NODE,
        )?;
        let mut common_prefix_lengths = vec![0; 1];
        let points = create_random_points(
            random,
            &config,
            max_doc,
            &mut common_prefix_lengths,
            is_doc_id_incremental,
        );
        let mut reader = DummyPointsReader::new(&points);
        MutablePointTreeReaderUtils::sort(&config, max_doc, &mut reader, 0, points.len())?;
        let mut sorted_points = points.clone();
        sorted_points.sort_by(|o1, o2| {
            let cmp = o1.packed_value.cmp(&o2.packed_value);
            if cmp == std::cmp::Ordering::Equal {
                o1.doc.cmp(&o2.doc)
            } else {
                cmp
            }
        });
        assert_ne!(points.as_ptr(), reader.points.as_ptr());
        assert_eq!(sorted_points.len(), reader.points.len());

        let mut prev_point: Option<&Point> = None;
        for (sorted_point, reader_point) in sorted_points.iter().zip(reader.points.iter()) {
            assert_eq!(sorted_point.packed_value, reader_point.packed_value);
            if let Some(prev) = prev_point
                && reader_point.packed_value == prev.packed_value
            {
                assert!(
                    reader_point.doc >= prev.doc,
                    "Doc IDs not in ascending order"
                );
            }
            prev_point = Some(reader_point);
        }
        Ok(())
    }

    #[test]
    fn test_sort_by_dim() -> Result<()> {
        let mut random = random();
        for _ in 0..5 {
            do_test_sort_by_dim(&mut random)?;
        }
        Ok(())
    }

    fn do_test_sort_by_dim<R: Rng + ?Sized>(random: &mut R) -> Result<()> {
        let config = Rc::new(create_random_config(random)?);
        let end = 1 << random.random_range(0..30);
        let max_doc = TestUtil::next_int(random, 1, end);
        let mut common_prefix_lengths = vec![0; config.num_dims as usize];
        let points =
            create_random_points(random, &config, max_doc, &mut common_prefix_lengths, false);
        let sorted_dim = random.random_range(0..config.num_index_dims);
        let mut reader = DummyPointsReader::new(&points);
        MutablePointTreeReaderUtils::sort_by_dim(
            &config,
            sorted_dim,
            &common_prefix_lengths,
            &mut reader,
            0,
            points.len(),
            &mut BytesRef::default(),
            &mut BytesRef::default(),
        )?;
        let offset = sorted_dim * config.bytes_per_dim;
        for i in 1..points.len() {
            let previous_value = &reader.points[i - 1].packed_value;
            let current_value = &reader.points[i].packed_value;

            let dim_start_prev = previous_value.offset + offset as usize;
            let dim_end_prev = dim_start_prev + config.bytes_per_dim as usize;
            let dim_start_curr = current_value.offset + offset as usize;
            let dim_end_curr = dim_start_curr + config.bytes_per_dim as usize;

            let mut cmp = compare_unsigned(
                &previous_value.bytes[dim_start_prev..dim_end_prev],
                &current_value.bytes[dim_start_curr..dim_end_curr],
            );

            if cmp == 0 {
                let data_dim_offset = config.packed_index_bytes_length();
                let data_dims_length =
                    (config.num_dims - config.num_index_dims) * config.bytes_per_dim;
                let data_start_prev = previous_value.offset + data_dim_offset as usize;
                let data_end_prev = data_start_prev + data_dims_length as usize;
                let data_start_curr = current_value.offset + data_dim_offset as usize;
                let data_end_curr = data_start_curr + data_dims_length as usize;

                cmp = compare_unsigned(
                    &previous_value.bytes[data_start_prev..data_end_prev],
                    &current_value.bytes[data_start_curr..data_end_curr],
                );
                if cmp == 0 {
                    cmp = reader.points[i - 1].doc - reader.points[i].doc;
                }
            }
            assert!(cmp <= 0);
        }
        Ok(())
    }

    #[test]
    fn test_partition() -> Result<()> {
        let mut random = random();
        for _ in 0..5 {
            do_test_partition(&mut random)?;
        }
        Ok(())
    }
    fn do_test_partition<R: Rng + ?Sized>(random: &mut R) -> Result<()> {
        let config = Rc::new(create_random_config(random)?);
        let mut common_prefix_lengths = vec![0; config.num_dims as usize];
        let end = 1 << random.random_range(0..30);
        let max_doc = TestUtil::next_int(random, 1, end);
        let points =
            create_random_points(random, &config, max_doc, &mut common_prefix_lengths, false);
        let split_dim = random.random_range(0..config.num_index_dims);
        let mut reader = DummyPointsReader::new(&points);
        let pivot = TestUtil::next_int(random, 0, points.len() as i32 - 1) as usize;

        MutablePointTreeReaderUtils::partition(
            &config,
            max_doc,
            split_dim,
            common_prefix_lengths[split_dim],
            &mut reader,
            0,
            points.len(),
            pivot,
            &mut BytesRef::default(),
            &mut BytesRef::default(),
        )?;
        let pivot_point = &reader.points[pivot as usize];
        let pivot_value = &pivot_point.packed_value;
        let offset = split_dim * config.bytes_per_dim;

        for i in 0..points.len() {
            let value = &reader.points[i].packed_value;
            let dim_start = value.offset + offset as usize;
            let dim_end = value.offset + (offset + config.bytes_per_dim) as usize;
            let pivot_dim_start = pivot_value.offset + offset as usize;
            let pivot_dim_end = pivot_value.offset + (offset + config.bytes_per_dim) as usize;

            let mut cmp = compare_unsigned(
                &value.bytes[dim_start..dim_end],
                &pivot_value.bytes[pivot_dim_start..pivot_dim_end],
            );
            if cmp == 0 {
                let data_dim_offset = config.packed_index_bytes_length();
                let data_dims_length =
                    (config.num_dims - config.num_index_dims) * config.bytes_per_dim;
                let data_start = value.offset + data_dim_offset as usize;
                let data_end = data_start + data_dims_length as usize;
                let pivot_data_start = pivot_value.offset + data_dim_offset as usize;
                let pivot_data_end = pivot_data_start + data_dims_length as usize;
                cmp = compare_unsigned(
                    &value.bytes[data_start..data_end],
                    &pivot_value.bytes[pivot_data_start..pivot_data_end],
                );
                if cmp == 0 {
                    cmp = reader.points[i].doc - pivot_point.doc;
                }
            }
            match i.cmp(&pivot) {
                std::cmp::Ordering::Less => {
                    assert!(cmp <= 0, "Expected cmp <= 0 for i < pivot, got {}", cmp);
                },
                std::cmp::Ordering::Greater => {
                    assert!(cmp >= 0, "Expected cmp >= 0 for i > pivot, got {}", cmp);
                },
                std::cmp::Ordering::Equal => {
                    assert_eq!(cmp, 0, "Expected cmp == 0 for the pivot index");
                },
            }
        }
        Ok(())
    }

    fn compare_unsigned(a: &[u8], b: &[u8]) -> i32 {
        a.cmp(b).to_int()
    }

    fn create_random_config<R: Rng + ?Sized>(random: &mut R) -> Result<BKDConfig> {
        let num_index_dims =
            TestUtil::next_int(random, 1, BKDConfig::MAX_INDEX_DIMS as i32) as usize;
        let num_dims =
            TestUtil::next_int(random, num_index_dims as i32, BKDConfig::MAX_DIMS as i32) as usize;
        let bytes_per_dim = TestUtil::next_int(random, 1, 16) as usize;
        let max_points_in_leaf_node = TestUtil::next_int(random, 50, 2000) as usize;
        BKDConfig::new(
            num_dims,
            num_index_dims,
            bytes_per_dim,
            max_points_in_leaf_node,
        )
    }
    fn create_random_points<R: Rng + ?Sized>(
        random: &mut R,
        config: &BKDConfig,
        max_doc: i32,
        common_prefix_lengths: &mut [usize],
        is_doc_id_incremental: bool,
    ) -> Vec<Point> {
        assert_eq!(common_prefix_lengths.len(), config.num_dims);
        let num_points = TestUtil::next_int(random, 1, 100000);
        let mut points: Vec<Point> = Vec::with_capacity(num_points as usize);
        if random.random_range(0..10) != 0 {
            for i in 0..num_points {
                let mut value = vec![0u8; config.packed_bytes_length()];
                random.fill_bytes(&mut value);
                let doc = if is_doc_id_incremental {
                    i.min(max_doc - 1)
                } else {
                    random.random_range(0..max_doc)
                };
                points.push(Point::new(random, &value, doc));
            }
            common_prefix_lengths.iter_mut().for_each(|prefix| {
                *prefix = TestUtil::next_int(random, 0, config.bytes_per_dim as i32) as usize
            });

            let first_value = points[0].packed_value.clone();
            for point in points.iter_mut().skip(1) {
                #[allow(clippy::needless_range_loop)]
                for dim in 0..config.num_dims {
                    let offset = dim * config.bytes_per_dim;
                    let prefix_len = common_prefix_lengths[dim];
                    let src_start = first_value.offset + offset;
                    let dst_start = point.packed_value.offset + offset;
                    point.packed_value.bytes.copy_from(
                        &first_value.bytes[src_start..src_start + prefix_len],
                        dst_start,
                    );
                }
            }
        } else {
            let num_data_dims = config.num_dims - config.num_index_dims;
            let mut index_dims = vec![0u8; config.packed_index_bytes_length()];
            random.fill_bytes(&mut index_dims);
            let data_dims_len = num_data_dims * config.bytes_per_dim;
            let mut data_dims = vec![0u8; data_dims_len];

            for i in 0..num_points {
                let mut value = vec![0u8; config.packed_bytes_length()];
                value.copy_from(&index_dims, 0);
                random.fill_bytes(&mut data_dims);
                let start = config.packed_index_bytes_length();
                value.copy_from(&data_dims, start);
                let doc = if is_doc_id_incremental {
                    i.min(max_doc - 1)
                } else {
                    random.random_range(0..max_doc)
                };
                points.push(Point::new(random, &value, doc));
            }
            common_prefix_lengths
                .iter_mut()
                .take(config.num_index_dims)
                .for_each(|prefix| *prefix = config.bytes_per_dim);

            common_prefix_lengths[config.num_index_dims..config.num_dims]
                .iter_mut()
                .for_each(|prefix| {
                    *prefix = TestUtil::next_int(random, 0, config.bytes_per_dim as i32) as usize
                });

            let first_value = points[0].packed_value.clone();
            for point in points.iter_mut().skip(1) {
                #[allow(clippy::needless_range_loop)]
                for dim in config.num_index_dims..config.num_dims {
                    let offset = dim * config.bytes_per_dim;
                    let prefix_len = common_prefix_lengths[dim];
                    let src_start = first_value.offset + offset;
                    let dst_start = point.packed_value.offset + offset;
                    point.packed_value.bytes.copy_from(
                        &first_value.bytes[src_start..src_start + prefix_len],
                        dst_start,
                    );
                }
            }
        }
        points
    }
    #[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
    struct Point {
        pub packed_value: BytesRef<Vec<u8>>,
        pub doc: i32,
    }

    impl Point {
        fn new<R: Rng + ?Sized>(random: &mut R, packed_value: &[u8], doc: i32) -> Self {
            let mut vec = vec![0u8; packed_value.len() + 1];
            vec[0] = random.random_range(0..255u8);
            vec.copy_from(packed_value, 1);
            Self {
                packed_value: BytesRef {
                    bytes: vec,
                    offset: 1,
                    length: packed_value.len(),
                },
                doc,
            }
        }
    }

    impl fmt::Display for Point {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            // Using Debug formatting for BytesRef.
            write!(f, "value={:?} doc={}", self.packed_value, self.doc)
        }
    }

    #[derive(Clone)]
    pub struct DummyPointsReader {
        points: Vec<Point>,
        temp: Vec<Point>,
    }
    impl TryClone for DummyPointsReader {
        fn try_clone(&self) -> Result<Self> {
            Ok(self.clone())
        }
    }

    impl DummyPointsReader {
        fn new(points: &[Point]) -> Self {
            Self {
                points: points.to_vec(),
                temp: vec![Point::default(); points.len()],
            }
        }
    }
    impl PointTree for DummyPointsReader {
        fn move_to_child(&mut self) -> Result<bool> {
            Ok(false)
        }

        fn move_to_sibling(&mut self) -> Result<bool> {
            Ok(false)
        }

        fn move_to_parent(&mut self) -> Result<bool> {
            Ok(false)
        }
    }
    impl MutablePointTree for DummyPointsReader {
        fn get_value(&self, i: usize, packed_value: &mut BytesRef<Vec<u8>>) {
            let point = &self.points[i].packed_value;
            packed_value.bytes = point.bytes.clone();
            packed_value.offset = point.offset;
            packed_value.length = point.length;
        }

        fn get_byte_at(&self, i: usize, k: usize) -> u8 {
            let packed_value = &self.points[i].packed_value;
            packed_value.bytes[packed_value.offset + k]
        }

        fn get_doc_id(&self, i: usize) -> i32 {
            self.points[i].doc
        }

        fn swap(&mut self, i: usize, j: usize) {
            self.points.swap(i, j);
        }

        fn save(&mut self, i: usize, j: usize) {
            self.temp[j] = self.points[i].clone();
        }

        fn restore(&mut self, i: usize, j: usize) {
            self.points[i..j].clone_from_slice(&self.temp[i..j]);
        }
    }
}
