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
use crate::core::index::{BytesRef, BytesRefBuilder};
use crate::core::util::error::lucene_error::Result;
use crate::core::util::{
    BytesRefComparator, Comparator, MSBRadixSorter, MSBRadixSorterBase, MergeSorter, Sorter,
    StableMSBRadixSorter, StableMSBRadixSorterBase, StringSorterBase,
};

pub(crate) struct StableStringSorter<T>
where
    T: StableStringSorterBase,
{
    delegate: T,
}
impl<T> StableStringSorter<T>
where
    T: StableStringSorterBase,
{
    pub fn new(delegate: T) -> StableStringSorter<T> {
        StableStringSorter { delegate }
    }
}

impl<T> Sorter for StableStringSorter<T> where T: StableStringSorterBase {}

impl<T> StringSorterBase for StableStringSorter<T>
where
    T: StableStringSorterBase + MSBRadixSorterBase,
{
    fn get(
        &mut self,
        builder: &mut BytesRefBuilder<Vec<u8>>,
        result: &mut BytesRef<Vec<u8>>,
        i: i32,
    ) -> Result<()> {
        self.delegate.get(builder, result, i)
    }

    fn fall_back_sorter<'a, C1>(&'a mut self, cmp: &'a mut C1, k: Option<i32>) -> impl Sorter + 'a
    where
        C1: BytesRefComparator + Comparator<BytesRef<Vec<u8>>>,
    {
        fall_back_sorter_stable(cmp, &mut self.delegate, k)
    }

    fn radix_sorter<'a, C>(&'a mut self, cmp: &'a mut C) -> impl Sorter + 'a
    where
        C: BytesRefComparator,
    {
        let length = cmp.compared_bytes_count();
        let delegate = StableMSBRadixSorterImpl {
            delegate: &mut self.delegate,
            cmp,
            scratch1: BytesRefBuilder::new(),
            scratch_bytes1: BytesRef::default(),
        };
        let stable_msb_radix_sorter = StableMSBRadixSorter::new(delegate, length as usize);
        MSBRadixSorter::new(length, stable_msb_radix_sorter)
    }
}
pub struct StableMSBRadixSorterImpl<'a, T, C>
where
    T: StableStringSorterBase + MSBRadixSorterBase,
    C: BytesRefComparator,
{
    delegate: &'a mut T,
    cmp: &'a mut C,
    scratch1: BytesRefBuilder<Vec<u8>>,
    scratch_bytes1: BytesRef<Vec<u8>>,
}
impl<T, C> Sorter for StableMSBRadixSorterImpl<'_, T, C>
where
    T: StableStringSorterBase + MSBRadixSorterBase,
    C: BytesRefComparator,
{
    fn swap(&mut self, i: usize, j: usize) -> Result<()> {
        self.delegate.swap(i, j)
    }
}

impl<T, C> MSBRadixSorterBase for StableMSBRadixSorterImpl<'_, T, C>
where
    T: StableStringSorterBase + MSBRadixSorterBase,
    C: BytesRefComparator,
{
    fn byte_at(&mut self, i: i32, k: i32) -> Result<i32> {
        self.delegate
            .get(&mut self.scratch1, &mut self.scratch_bytes1, i)?;
        self.cmp.byte_at(&self.scratch_bytes1, k)
    }

    fn get_fallback_sorter(&mut self, k: i32, _length: i32) -> impl Sorter {
        fall_back_sorter_stable(self.cmp, self.delegate, Some(k))
    }
}

impl<T, C> StableMSBRadixSorterBase for StableMSBRadixSorterImpl<'_, T, C>
where
    T: StableStringSorterBase + MSBRadixSorterBase,
    C: BytesRefComparator,
{
    fn save(&mut self, i: i32, j: i32) {
        self.delegate.save(i, j)
    }

    fn restore(&mut self, i: i32, j: i32) {
        self.delegate.restore(i, j)
    }
}

pub struct MergeSorterStableImpl<'a, T, C>
where
    T: StableStringSorterBase + MSBRadixSorterBase,
    C: BytesRefComparator,
{
    scratch1: BytesRefBuilder<Vec<u8>>,
    scratch2: BytesRefBuilder<Vec<u8>>,
    scratch_bytes1: BytesRef<Vec<u8>>,
    scratch_bytes2: BytesRef<Vec<u8>>,
    cmp: &'a mut C,
    delegate: &'a mut T,
    k: Option<i32>,
}
impl<T, C> Sorter for MergeSorterStableImpl<'_, T, C>
where
    T: StableStringSorterBase + MSBRadixSorterBase,
    C: BytesRefComparator,
{
    fn compare(&mut self, i: usize, j: usize) -> Result<i32> {
        self.delegate
            .get(&mut self.scratch1, &mut self.scratch_bytes1, i as i32)?;
        self.delegate
            .get(&mut self.scratch2, &mut self.scratch_bytes2, j as i32)?;
        if self.k.is_some() {
            self.cmp.compare_with_offset(
                &self.scratch_bytes1,
                &self.scratch_bytes2,
                self.k.unwrap(),
            )
        } else {
            self.cmp.compare(&self.scratch_bytes1, &self.scratch_bytes2)
        }
    }

    fn swap(&mut self, i: usize, j: usize) -> Result<()> {
        self.delegate.swap(i, j)
    }
}

impl<T, C> StringSorterBase for MergeSorterStableImpl<'_, T, C>
where
    C: BytesRefComparator,
    T: StableStringSorterBase + MSBRadixSorterBase,
{
    fn get(
        &mut self,
        builder: &mut BytesRefBuilder<Vec<u8>>,
        result: &mut BytesRef<Vec<u8>>,
        i: i32,
    ) -> Result<()> {
        self.delegate.get(builder, result, i)
    }
}

impl<T, C> StableStringSorterBase for MergeSorterStableImpl<'_, T, C>
where
    T: StableStringSorterBase + MSBRadixSorterBase,
    C: BytesRefComparator,
{
    fn save(&mut self, i: i32, j: i32) {
        self.delegate.save(i, j)
    }

    fn restore(&mut self, i: i32, j: i32) {
        self.delegate.restore(i, j)
    }
}

impl<T, C> MSBRadixSorterBase for MergeSorterStableImpl<'_, T, C>
where
    C: BytesRefComparator,
    T: StableStringSorterBase + MSBRadixSorterBase,
{
    fn byte_at(&mut self, i: i32, k: i32) -> Result<i32> {
        self.delegate.byte_at(i, k)
    }

    fn get_fallback_sorter(&mut self, k: i32, length: i32) -> impl Sorter {
        self.delegate.get_fallback_sorter(k, length)
    }
}

impl<T, C> StableMSBRadixSorterBase for MergeSorterStableImpl<'_, T, C>
where
    T: StableStringSorterBase + MSBRadixSorterBase,
    C: BytesRefComparator,
{
    fn save(&mut self, i: i32, j: i32) {
        self.delegate.save(i, j)
    }

    fn restore(&mut self, i: i32, j: i32) {
        self.delegate.restore(i, j)
    }
}

pub trait StableStringSorterBase: StringSorterBase {
    /// Save the i-th value into the j-th position in temporary storage.
    fn save(&mut self, i: i32, j: i32);
    /// Restore values between i-th and j-th(excluding) in temporary storage
    /// into original storage.
    fn restore(&mut self, i: i32, j: i32);
}

fn fall_back_sorter_stable<'a, T, C>(
    cmp: &'a mut C,
    sorter: &'a mut T,
    k: Option<i32>,
) -> impl Sorter + use<'a, T, C>
where
    T: StableStringSorterBase + MSBRadixSorterBase,
    C: BytesRefComparator,
{
    let delegate = MergeSorterStableImpl {
        scratch1: BytesRefBuilder::new(),
        scratch2: BytesRefBuilder::new(),
        scratch_bytes1: BytesRef::default(),
        scratch_bytes2: BytesRef::default(),
        cmp,
        delegate: sorter,
        k,
    };
    MergeSorter {
        delegate,
        pivot_index: 0,
    }
}
