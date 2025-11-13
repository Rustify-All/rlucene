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
use std::fmt::Display;

use crate::core::util::accountable::Accountable;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::long_values::LongValues;
use crate::core::util::packed::mutable_enum::MutableEnum;
use crate::core::util::packed::paged_growable_writer::PagedGrowableWriter;
use crate::core::util::packed::paged_mutable::PagedMutable;
use crate::core::util::packed::{DummyMutable, Mutable, PackedInts, Reader};

const MIN_BLOCK_SIZE: i32 = 1 << 6;
const MAX_BLOCK_SIZE: i32 = 1 << 30;
/// Base implementation for
/// [`PagedMutable`](PagedMutable) and
/// [`PagedGrowableWriter`](PagedGrowableWriter).
///
///
/// # Lucene Internal
/// This is an internal utility for use within the Lucene system.
#[derive(Default)]
pub(crate) struct AbstractPagedMutable<T>
where
    T: AbstractPagedMutableBase,
{
    sub_reader: T,
    size: i64,
    page_shift: i32,
    page_mask: i32,
    sub_mutables: Vec<MutableEnum>,
}

impl<T> AbstractPagedMutable<T>
where
    T: AbstractPagedMutableBase,
{
    pub fn new(size: i64, page_size: i32, sub_reader: T) -> Result<AbstractPagedMutable<T>> {
        let page_shift = PackedInts::check_block_size(page_size, MIN_BLOCK_SIZE, MAX_BLOCK_SIZE)?;
        let page_mask = page_size - 1;
        let num_pages = PackedInts::num_blocks(size, page_size)?;
        let mut sub_mutables = Vec::with_capacity(num_pages as usize);
        // We use index-based access to sub_mutables, so we can initialize it as
        // DummyMutable.
        for _ in 0..num_pages as usize {
            sub_mutables.push(MutableEnum::Dummy(DummyMutable));
        }
        let mut result = AbstractPagedMutable {
            sub_reader,
            size,
            page_shift,
            page_mask,
            sub_mutables,
        };
        if result.sub_reader.fill_pages() {
            result.fill_pages()?;
        };
        Ok(result)
    }
    pub fn fill_pages(&mut self) -> Result<()> {
        let num_pages = PackedInts::num_blocks(self.size, self.page_size())?;
        for i in 0..num_pages {
            // do not allocate for more entries than necessary on the last page
            let value_count = if i == num_pages - 1 {
                self.last_page_size(self.size)
            } else {
                self.page_size()
            };
            self.sub_mutables[i as usize] = self
                .sub_reader
                .new_mutable(value_count, self.sub_reader.bits_per_value());
        }
        Ok(())
    }
    fn last_page_size(&self, size: i64) -> i32 {
        let sz = self.index_in_page(size);
        if sz == 0 { self.page_size() } else { sz }
    }
    fn page_size(&self) -> i32 {
        self.page_mask + 1
    }
    pub fn size(&self) -> i64 {
        self.size
    }
    fn page_index(&self, index: i64) -> usize {
        (index >> self.page_shift) as usize
    }

    fn index_in_page(&self, index: i64) -> i32 {
        (index & self.page_mask as i64) as i32
    }
    /// Sets the value at the specified index.
    pub fn set(&mut self, index: i64, value: i64) {
        debug_assert!(
            index < self.size,
            "Index out of bounds: index={} size={}",
            index,
            self.size
        );
        let page_index = self.page_index(index);
        let index_in_page = self.index_in_page(index);
        self.sub_mutables[page_index].set(index_in_page, value)
    }
    pub(crate) fn base_ram_bytes_used(&self) -> i64 {
        self.sub_reader.base_ram_bytes_used_base()
    }
    /// Create a new copy of size <code>newSize</code> based on the content of
    /// this buffer. This is much more efficient than creating a new
    /// instance and copying values one by one.
    pub fn resize(&self, new_size: i64) -> Result<AbstractPagedMutable<T>> {
        let sub = self.sub_reader.new_unfilled_copy();
        let mut copy = AbstractPagedMutable::new(new_size, self.page_size(), sub)?;
        let num_common_pages = std::cmp::min(copy.sub_mutables.len(), self.sub_mutables.len());
        let mut copy_buffer = vec![0i64; 1024];
        for i in 0..copy.sub_mutables.len() {
            // Determine the number of values in the current page
            let value_count = if i == copy.sub_mutables.len() - 1 {
                self.last_page_size(new_size)
            } else {
                self.page_size()
            };
            let bpv = if i < num_common_pages {
                self.sub_mutables[i].get_bits_per_value()
            } else {
                self.sub_reader.bits_per_value()
            };
            copy.sub_mutables[i] = self.sub_reader.new_mutable(value_count, bpv);

            if i < num_common_pages {
                let copy_length = std::cmp::min(value_count, self.sub_mutables[i].size());
                PackedInts::copy_with_buffer(
                    &self.sub_mutables[i],
                    0,
                    &mut copy.sub_mutables[i],
                    0,
                    copy_length,
                    &mut copy_buffer,
                );
            }
        }
        Ok(copy)
    }
    pub fn grow_with_size(&self, min_size: i64) -> Result<Option<AbstractPagedMutable<T>>> {
        if min_size <= self.size {
            return Ok(None);
        }
        let mut extra = min_size >> 3;
        if extra < 3 {
            extra = 3;
        }
        let new_size = min_size + extra;
        Ok(Some(self.resize(new_size)?))
    }

    pub fn grow(&self) -> Result<Option<AbstractPagedMutable<T>>> {
        self.grow_with_size(self.size() << 1)
    }
}
impl<T> LongValues for AbstractPagedMutable<T>
where
    T: AbstractPagedMutableBase,
{
    fn get(&self, index: i64) -> Result<i64> {
        debug_assert!(index < self.size, "index={} size={}", index, self.size);
        let page_index = self.page_index(index);
        let index_in_page = self.index_in_page(index);
        Ok(self.sub_mutables[page_index].get(index_in_page))
    }
}
impl<T> Accountable for AbstractPagedMutable<T>
where
    T: AbstractPagedMutableBase,
{
    fn ram_bytes_used(&self) -> Result<i64> {
        let mut byte_used = self.base_ram_bytes_used();
        for sub_mutable in &self.sub_mutables {
            byte_used += sub_mutable.ram_bytes_used()?;
        }
        Ok(byte_used)
    }
}
impl<T> Display for AbstractPagedMutable<T>
where
    T: AbstractPagedMutableBase + Display,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}(size={}, pageSize={})",
            self.sub_reader,
            self.size,
            self.page_size()
        )
    }
}
pub(crate) trait AbstractPagedMutableBase {
    fn new_mutable(&self, value_count: i32, bits_per_value: i32) -> MutableEnum;
    fn new_unfilled_copy(&self) -> Self
    where
        Self: Sized;
    fn base_ram_bytes_used_base(&self) -> i64;
    fn fill_pages(&self) -> bool;
    fn bits_per_value(&self) -> i32;
}

pub enum AbstractPagedMutableBaseEnum {
    Mutable(PagedMutable),
    GrowableWriter(PagedGrowableWriter),
}
impl Default for AbstractPagedMutableBaseEnum {
    /// for padding using
    fn default() -> Self {
        AbstractPagedMutableBaseEnum::Mutable(PagedMutable::default())
    }
}
impl AbstractPagedMutableBase for AbstractPagedMutableBaseEnum {
    fn new_mutable(&self, value_count: i32, bits_per_value: i32) -> MutableEnum {
        match self {
            AbstractPagedMutableBaseEnum::Mutable(m) => m.new_mutable(value_count, bits_per_value),
            AbstractPagedMutableBaseEnum::GrowableWriter(g) => {
                g.new_mutable(value_count, bits_per_value)
            },
        }
    }

    fn new_unfilled_copy(&self) -> Self {
        match self {
            AbstractPagedMutableBaseEnum::Mutable(m) => {
                AbstractPagedMutableBaseEnum::Mutable(m.new_unfilled_copy())
            },
            AbstractPagedMutableBaseEnum::GrowableWriter(g) => {
                AbstractPagedMutableBaseEnum::GrowableWriter(g.new_unfilled_copy())
            },
        }
    }

    fn base_ram_bytes_used_base(&self) -> i64 {
        match self {
            AbstractPagedMutableBaseEnum::Mutable(m) => m.base_ram_bytes_used_base(),
            AbstractPagedMutableBaseEnum::GrowableWriter(g) => g.base_ram_bytes_used_base(),
        }
    }

    fn fill_pages(&self) -> bool {
        match self {
            AbstractPagedMutableBaseEnum::Mutable(m) => m.fill_pages(),
            AbstractPagedMutableBaseEnum::GrowableWriter(g) => g.fill_pages(),
        }
    }

    fn bits_per_value(&self) -> i32 {
        match self {
            AbstractPagedMutableBaseEnum::Mutable(m) => m.bits_per_value(),
            AbstractPagedMutableBaseEnum::GrowableWriter(g) => g.bits_per_value(),
        }
    }
}
