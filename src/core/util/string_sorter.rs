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
use crate::core::util::bytes_ref_comparator::{BYTES_REF_COMPARATOR_TYPE, BytesRefComparator};
use crate::core::util::error::lucene_error::Result;
use crate::core::util::intro_sorter::IntroSorter;
use crate::core::util::{MSBRadixSorter, MSBRadixSorterBase, Sorter};

/// A [`BytesRef`] sorter that attempts to use an efficient radix sorter if
/// [`StringSorter::compare`] is a [`BytesRefComparator`]. Otherwise, it falls
/// back to [`StringSorterBase::fall_back_sorter`].
///
/// # Note
/// - This is an internal API and is not intended for external use.
pub(crate) struct StringSorter<T, C>
where
    T: StringSorterBase,
    C: BytesRefComparator,
{
    delegate: T,
    scratch1: BytesRefBuilder<Vec<u8>>,
    scratch2: BytesRefBuilder<Vec<u8>>,
    scratch_bytes1: BytesRef<Vec<u8>>,
    scratch_bytes2: BytesRef<Vec<u8>>,
    cmp: C,
}

impl<T, C> StringSorter<T, C>
where
    T: StringSorterBase,
    C: BytesRefComparator,
{
    pub(crate) fn new(delegate: T, cmp: C) -> StringSorter<T, C> {
        StringSorter {
            delegate,
            scratch1: BytesRefBuilder::default(),
            scratch2: BytesRefBuilder::default(),
            scratch_bytes1: BytesRef::default(),
            scratch_bytes2: BytesRef::default(),
            cmp,
        }
    }
    #[cfg(debug_assertions)]
    pub(crate) fn get_delegate(&self) -> &T {
        &self.delegate
    }
}

impl<T, C> Sorter for StringSorter<T, C>
where
    T: StringSorterBase,
    C: BytesRefComparator,
{
    fn compare(&mut self, i: usize, j: usize) -> Result<i32> {
        self.delegate
            .get(&mut self.scratch1, &mut self.scratch_bytes1, i as i32)?;
        self.delegate
            .get(&mut self.scratch2, &mut self.scratch_bytes2, j as i32)?;
        self.cmp.compare(&self.scratch_bytes1, &self.scratch_bytes2)
    }

    fn swap(&mut self, i: usize, j: usize) -> Result<()> {
        self.delegate.swap(i, j)
    }

    fn sort(&mut self, from: i32, to: i32) -> Result<()> {
        // In fact, it is necessary to provide an instance that implements
        // BytesRefComparator to simplify the code. However, the TYPE of
        // this instance cannot be specified as "BytesRefComparator".
        if C::TYPE.eq(BYTES_REF_COMPARATOR_TYPE) {
            self.delegate.radix_sorter(&mut self.cmp).sort(from, to)
        } else {
            self.delegate
                .fall_back_sorter(&mut self.cmp, None)
                .sort(from, to)
        }
    }
}

pub struct MSBStringRadixSorter<'a, T, C>
where
    T: StringSorterBase,
    C: BytesRefComparator,
{
    scratch1: BytesRefBuilder<Vec<u8>>,
    scratch_bytes1: BytesRef<Vec<u8>>,
    cmp: &'a mut C,
    delegate: &'a mut T,
}
impl<'a, T, C> MSBStringRadixSorter<'a, T, C>
where
    T: StringSorterBase,
    C: BytesRefComparator,
{
    pub fn new(cmp: &'a mut C, delegate: &'a mut T) -> MSBStringRadixSorter<'a, T, C> {
        MSBStringRadixSorter {
            scratch1: BytesRefBuilder::default(),
            scratch_bytes1: BytesRef::default(),
            cmp,
            delegate,
        }
    }
}

impl<T, C> Sorter for MSBStringRadixSorter<'_, T, C>
where
    T: StringSorterBase,
    C: BytesRefComparator,
{
    fn swap(&mut self, i: usize, j: usize) -> Result<()> {
        self.delegate.swap(i, j)
    }
}

impl<T, C> MSBRadixSorterBase for MSBStringRadixSorter<'_, T, C>
where
    T: StringSorterBase,
    C: BytesRefComparator,
{
    fn byte_at(&mut self, i: i32, k: i32) -> Result<i32> {
        self.delegate
            .get(&mut self.scratch1, &mut self.scratch_bytes1, i)?;
        self.cmp.byte_at(&self.scratch_bytes1, k)
    }

    fn get_fallback_sorter(&mut self, k: i32, _length: i32) -> impl Sorter {
        self.delegate.fall_back_sorter(self.cmp, Some(k))
    }
}

pub struct IntroSorterImpl<'a, T, C>
where
    T: StringSorterBase,
    C: BytesRefComparator,
{
    pivot: BytesRef<Vec<u8>>,
    pivot_builder: BytesRefBuilder<Vec<u8>>,
    scratch1: BytesRefBuilder<Vec<u8>>,
    scratch2: BytesRefBuilder<Vec<u8>>,
    scratch_bytes1: BytesRef<Vec<u8>>,
    scratch_bytes2: BytesRef<Vec<u8>>,
    cmp: &'a mut C,
    delegate: &'a mut T,
    k: Option<i32>,
}
impl<'a, T, C> IntroSorterImpl<'a, T, C>
where
    T: StringSorterBase,
    C: BytesRefComparator,
{
    pub fn new(cmp: &'a mut C, delegate: &'a mut T, k: Option<i32>) -> IntroSorterImpl<'a, T, C> {
        IntroSorterImpl {
            pivot: BytesRef::default(),
            pivot_builder: BytesRefBuilder::default(),
            scratch1: BytesRefBuilder::default(),
            scratch2: BytesRefBuilder::default(),
            scratch_bytes1: BytesRef::default(),
            scratch_bytes2: BytesRef::default(),
            cmp,
            delegate,
            k,
        }
    }
}
impl<T, C> Sorter for IntroSorterImpl<'_, T, C>
where
    T: StringSorterBase,
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

    fn set_pivot(&mut self, i: i32) -> Result<()> {
        self.delegate
            .get(&mut self.pivot_builder, &mut self.pivot, i)?;
        Ok(())
    }

    fn compare_pivot(&mut self, j: i32) -> Result<i32> {
        self.delegate
            .get(&mut self.scratch1, &mut self.scratch_bytes1, j)?;
        if self.k.is_some() {
            self.cmp
                .compare_with_offset(&self.pivot, &self.scratch_bytes1, self.k.unwrap())
        } else {
            self.cmp.compare(&self.pivot, &self.scratch_bytes1)
        }
    }

    fn sort(&mut self, from: i32, to: i32) -> Result<()> {
        IntroSorter::sort_range(self, from, to)?;
        Ok(())
    }
}

impl<T, C> IntroSorter for IntroSorterImpl<'_, T, C>
where
    T: StringSorterBase,
    C: BytesRefComparator,
{
}

pub trait StringSorterBase: Sorter {
    fn get(
        &mut self,
        builder: &mut BytesRefBuilder<Vec<u8>>,
        result: &mut BytesRef<Vec<u8>>,
        i: i32,
    ) -> Result<()>;
    fn fall_back_sorter<'a, C>(&'a mut self, cmp: &'a mut C, k: Option<i32>) -> impl Sorter + 'a
    where
        C: BytesRefComparator,
        Self: Sorter + Sized,
    {
        IntroSorterImpl::new(cmp, self, k)
    }
    fn radix_sorter<'a, C>(&'a mut self, cmp: &'a mut C) -> impl Sorter + 'a
    where
        C: BytesRefComparator,
        Self: Sorter + Sized,
    {
        let length = cmp.compared_bytes_count();
        let delegate = MSBStringRadixSorter::new(cmp, self);
        MSBRadixSorter::new(length, delegate)
    }
}
