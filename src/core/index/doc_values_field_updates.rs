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
use parking_lot::Mutex;
use std::borrow::Cow;
use std::sync::Arc;

use crate::core::index::BytesRef;
use crate::core::index::binary_doc_values::BinaryDocValues;
use crate::core::index::binary_doc_values_field_updates::{
    AbstractIteratorBinary, BinaryDocValuesFieldUpdates,
};
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::numeric_doc_values_field_updates::{
    AbstractIteratorNumeric, NumericDocValuesFieldUpdates, SingleValueNumericDocValuesFieldUpdates,
};
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::util::accountable::Accountable;
use crate::core::util::bit_set::BitSet;
use crate::core::util::bit_set_iterator::BitSetIterator;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::intro_sorter::IntroSorter;
use crate::core::util::long_values::LongValues;
use crate::core::util::packed::abstract_paged_mutable::AbstractPagedMutable;
use crate::core::util::packed::mutable_packed64_enum::MutablePacked64Enum;
use crate::core::util::packed::paged_mutable::PagedMutable;
use crate::core::util::packed::{Mutable, PackedInts, Reader};
use crate::core::util::priority_queue::{Compare, PriorityQueue};
use crate::core::util::sparse_fixed_bit_set::SparseFixedBitSet;
use crate::core::util::{Sorter, ToInt};

/// Holds updates for a single DocValues field, for a set of documents within
/// one segment.
///
/// # Note
/// This is an experimental feature and may change in future versions.
pub(crate) struct DocValuesFieldUpdates<D>
where
    D: DocValuesFieldUpdatesBase,
{
    pub(crate) field: String,
    pub(crate) doc_values_type: DocValuesType,
    pub(crate) del_gen: i64,
    max_doc: i32,
    inner: Mutex<DocValuesFieldInner>,
    pub(crate) sub_update: D,
}
pub(crate) struct DocValuesFieldInner {
    finished: bool,
    pub docs: AbstractPagedMutable<PagedMutable>,
    pub(crate) size: i32,
    // for reused iterator
    pub docs_iter: Option<Arc<AbstractPagedMutable<PagedMutable>>>,
}

pub(crate) struct DocValuesFieldInnerIter {
    size: i32,
    // for reused iterator
    docs: Arc<AbstractPagedMutable<PagedMutable>>,
}

impl DocValuesFieldInner {
    pub(crate) fn new(bits_per_value: i32) -> Result<Self> {
        let sub_mutable =
            PagedMutable::with_overhead_ratio(PAGE_SIZE, bits_per_value, PackedInts::DEFAULT);
        let writer = AbstractPagedMutable::new(1, PAGE_SIZE, sub_mutable)?;
        Ok(Self {
            finished: false,
            docs: writer,
            size: 0,
            docs_iter: None,
        })
    }
    pub(crate) fn resize(&mut self, size: i32) -> Result<()> {
        self.docs = self.docs.resize(size as i64)?;
        Ok(())
    }
    pub(crate) fn grow(&mut self, size: i32) -> Result<()> {
        let result = self.docs.grow_with_size(size as i64)?;
        if let Some(docs) = result {
            self.docs = docs;
        }
        Ok(())
    }
    pub(crate) fn swap(&mut self, i: i32, j: i32) -> Result<()> {
        let tmp_doc = self.docs.get_immutable(j as i64)?;
        let value_i = self.docs.get_immutable(i as i64)?;
        self.docs.set(j as i64, value_i);
        self.docs.set(i as i64, tmp_doc);
        Ok(())
    }
}

impl<D> DocValuesFieldUpdates<D>
where
    D: DocValuesFieldUpdatesBase,
{
    pub(crate) fn new<T>(
        max_doc: i32,
        del_gen: i64,
        field: T,
        doc_values_type: DocValuesType,
        sub_update: D,
    ) -> Result<Self>
    where
        T: Into<String>,
    {
        let bits_per_value = PackedInts::bits_required(max_doc as i64 - 1)? + SHIFT;
        let inner = DocValuesFieldInner::new(bits_per_value)?;
        Ok(Self {
            field: field.into(),
            doc_values_type,
            del_gen,
            max_doc,
            inner: Mutex::new(inner),
            sub_update,
        })
    }

    fn get_finished(&self) -> Result<bool> {
        let inner = self.inner.lock();
        Ok(inner.finished)
    }
    /// # Warning
    /// In Java Lucene, these two methods are executed within the same critical
    /// section.However, from a logical perspective, this is not necessary.
    pub(crate) fn add_value(&mut self, doc: i32, value: i64) -> Result<()> {
        let index = if self.sub_update.need_add_doc() {
            self.add(doc)?
        } else {
            0
        };
        self.sub_update.add_value(doc, value, index)
    }
    /// # Warning
    /// In Java Lucene, these two methods are executed within the same critical
    /// section.However, from a logical perspective, this is not necessary.
    fn add_byte_ref(&mut self, doc: i32, value: &BytesRef<Vec<u8>>) -> Result<()> {
        let index = self.add(doc)?;
        self.sub_update.add_byte_ref(doc, value, index)
    }
    /// Returns an iterator for updated documents and their values.
    pub(crate) fn iterator(&self) -> Result<DocValuesFieldIteratorEnum> {
        self.ensure_finished()?;
        let inner = self.inner.lock();
        let v = DocValuesFieldInnerIter {
            size: inner.size,
            docs: inner.docs_iter.as_ref().unwrap().clone(),
        };
        self.sub_update.iterator(v, self.del_gen)
    }
    /// Adds the value for the given `doc_id`.
    ///
    /// This method prevents conditional calls to [`DocValuesFieldIterator::long_value`]
    /// or [`DocValuesFieldIterator::binary_value`], since the implementation knows
    /// whether it is a long value iterator or a binary value iterator.
    fn add_iterator<T>(&mut self, doc_id: i32, iterator: &mut T) -> Result<()>
    where
        T: DocValuesFieldIterator,
    {
        self.sub_update.add_iterator(doc_id, iterator)
    }
    pub(crate) fn finish(&mut self) -> Result<()> {
        let mut inner = self.inner.lock();
        if inner.finished {
            return Err(LuceneError::illegal_argument("already finished"));
        }
        inner.finished = true;
        let size = inner.size;
        // shrink wrap
        if (inner.size as i64) < inner.docs.size() {
            inner.resize(size)?;
            self.sub_update.resize(size)?;
        }

        if inner.size > 0 {
            // We need a stable sort but InPlaceMergeSorter performs lots of
            // swaps which hurt performance due to all the packed
            // ints we are using. Another option would be TimSorter,
            // but it needs additional API (copy to temp storage,
            // compare with item in temp storage, etc.), so we instead
            // use quicksort and record ords of each update to guarantee
            // stability.
            let mut ords = PackedInts::get_mutable(
                inner.size,
                PackedInts::bits_required((inner.size - 1) as i64)?,
                PackedInts::DEFAULT,
            );
            for i in 0..inner.size {
                ords.set(i, i as i64)
            }
            let mut sorter = IntroSorterImpl {
                ords: &mut ords,
                pivot_doc: 0,
                pivot_ord: 0,
                sub_update: &mut self.sub_update,
                inner: &mut inner,
            };
            sorter.sort(0, size)?;
        }
        inner.docs_iter = Some(Arc::new(std::mem::take(&mut inner.docs)));
        self.sub_update.finish();
        Ok(())
    }
    /// Returns true if this instance contains any updates.
    pub(crate) fn any(&self) -> bool {
        let inner = self.inner.lock();
        let result = inner.size > 0;
        if self.sub_update.need_any() {
            self.sub_update.any(result)
        } else {
            result
        }
    }
    /// Adds an update that resets the document value.
    pub(crate) fn reset(&mut self, doc: i32) -> Result<()> {
        if self.sub_update.need_reset() {
            self.sub_update.reset(doc)
        } else {
            self.add_internal(doc, HAS_NO_VALUE_MASK).map(|_| ())
        }
    }

    pub(crate) fn add(&mut self, doc: i32) -> Result<i32> {
        self.add_internal(doc, HAS_VALUE_MASK)
    }
    fn add_internal(&mut self, doc: i32, has_value_mask: i64) -> Result<i32> {
        let mut inner = self.inner.lock();
        if inner.finished {
            return Err(LuceneError::illegal_argument("already finished"));
        }
        let size = inner.size;
        debug_assert!(doc < self.max_doc, "doc must be less than max_doc");
        // TODO: if the Sorter interface changes to take long indexes, we can
        // remove that limitation
        if size == i32::MAX {
            return Err(LuceneError::illegal_state(
                "cannot support more than Integer.MAX_VALUE doc/value entries",
            ));
        }
        // grow the structures to have room for more elements
        if inner.docs.size() == size as i64 {
            inner.grow(size + 1)?;
            self.sub_update.grow(size + 1)?;
        }
        let value = ((doc as i64) << 1) | has_value_mask;
        inner.docs.set(size as i64, value);
        inner.size += 1;
        Ok(inner.size - 1)
    }
    // pub(crate) fn swap(&mut self, i: i32, j: i32) -> Result<()> {
    //     self.sub_update.swap(i, j)?;
    //     let mut inner = self.inner.lock();
    //     inner.swap(i, j)?;
    //     Ok(())
    // }
    pub(crate) fn grow(&mut self, size: i32) -> Result<()> {
        self.sub_update.grow(size)?;
        let mut inner = self.inner.lock();
        inner.grow(size)?;
        Ok(())
    }
    pub(crate) fn resize(&mut self, size: i32) -> Result<()> {
        self.sub_update.resize(size)?;
        let mut inner = self.inner.lock();
        inner.resize(size)?;
        Ok(())
    }
    pub(crate) fn ensure_finished(&self) -> Result<()> {
        let inner = self.inner.lock();
        if !inner.finished {
            return Err(LuceneError::illegal_state("call finish first"));
        }
        Ok(())
    }
}

impl<D> Accountable for DocValuesFieldUpdates<D>
where
    D: DocValuesFieldUpdatesBase,
{
    fn ram_bytes_used(&self) -> Result<i64> {
        todo!()
    }
}

pub(crate) trait DocValuesFieldUpdatesBase: Accountable {
    fn finish(&mut self);
    fn add_value(&mut self, doc: i32, value: i64, index: i32) -> Result<()>;
    fn add_byte_ref(&mut self, doc: i32, value: &BytesRef<Vec<u8>>, index: i32) -> Result<()>;
    fn add_iterator<T: DocValuesFieldIterator>(
        &mut self,
        doc_id: i32,
        iterator: &mut T,
    ) -> Result<()>;
    /// This method could be called once
    fn iterator(
        &self,
        inner: DocValuesFieldInnerIter,
        del_gen: i64,
    ) -> Result<DocValuesFieldIteratorEnum>;
    fn swap(&mut self, _i: i32, _j: i32) -> Result<()> {
        unimplemented!("must be implemented if you need to use it")
    }
    fn grow(&mut self, _size: i32) -> Result<()> {
        unimplemented!("must be implemented if you need to use it")
    }
    fn resize(&mut self, _size: i32) -> Result<()> {
        Ok(())
    }
    fn reset(&mut self, _doc: i32) -> Result<()> {
        unimplemented!("must be implemented if you need to use it")
    }
    fn need_reset(&self) -> bool {
        false
    }
    fn any(&self, _super_any: bool) -> bool {
        unimplemented!("must be implemented if you need to use it")
    }
    fn need_any(&self) -> bool {
        false
    }
    fn sub_type(&self) -> DocValuesType;
    fn need_add_doc(&self) -> bool {
        true
    }
}

struct IntroSorterImpl<'a, D>
where
    D: DocValuesFieldUpdatesBase,
{
    ords: &'a mut MutablePacked64Enum,
    pivot_doc: i64,
    pivot_ord: i64,
    sub_update: &'a mut D,
    inner: &'a mut DocValuesFieldInner,
}

impl<D> Sorter for IntroSorterImpl<'_, D>
where
    D: DocValuesFieldUpdatesBase,
{
    fn compare(&mut self, i: i32, j: i32) -> Result<i32> {
        // increasing docID order:
        // NOTE: we can have ties here, when the same docID was updated in the
        // same segment, in which case we rely on sort being
        // stable and preserving the original order so the last update to that
        // docID wins
        let cmp = (self.inner.docs.get_immutable(i as i64)? >> 1)
            .cmp(&(self.inner.docs.get_immutable(j as i64)? >> 1));

        if cmp == std::cmp::Ordering::Equal {
            Ok((self.ords.get(i) - self.ords.get(j)) as i32)
        } else {
            Ok(cmp.to_int())
        }
    }

    fn swap(&mut self, i: i32, j: i32) -> Result<()> {
        let tmp_ord = self.ords.get(i);
        let value = self.ords.get(j);
        self.ords.set(i, value);
        self.ords.set(j, tmp_ord);
        self.inner.swap(i, j)?;
        self.sub_update.swap(i, j)?;
        Ok(())
    }

    fn set_pivot(&mut self, i: i32) -> Result<()> {
        self.pivot_doc = self.inner.docs.get_immutable(i as i64)? >> 1;
        self.pivot_ord = self.ords.get(i);
        Ok(())
    }

    fn compare_pivot(&mut self, j: i32) -> Result<i32> {
        let mut cmp = self
            .pivot_doc
            .cmp(&((self.inner.docs.get_immutable(j as i64)? as u64 >> 1) as i64));
        if cmp == std::cmp::Ordering::Equal {
            // If docIDs are the same, compare pivot_ord with ords[j]
            cmp = (self.pivot_ord - self.ords.get(j)).cmp(&0);
        }
        Ok(cmp.to_int())
    }

    fn sort(&mut self, from: i32, to: i32) -> Result<()> {
        IntroSorter::sort_range(self, from, to)?;
        Ok(())
    }
}

impl<D> IntroSorter for IntroSorterImpl<'_, D> where D: DocValuesFieldUpdatesBase {}

/// An iterator over documents and their updated values.
///
/// Only documents with updates are returned by this iterator, and the documents
/// are returned in increasing order.
pub trait DocValuesFieldIterator: DocValuesIterator {
    /// Returns a long value for the current document if this iterator is a long
    /// iterator.
    fn long_value(&mut self) -> Result<i64>;

    /// Returns a binary value for the current document if this iterator is a
    /// binary value iterator.
    fn binary_value(&mut self) -> Result<Cow<'_, BytesRef<Vec<u8>>>>;

    /// Returns the delGen for this packet.
    fn del_gen(&self) -> i64;

    /// Returns true if this document has a value.
    fn has_value(&self) -> bool;
}
pub enum DocValuesFieldIteratorEnum {
    AbstractBinary(AbstractIterator<AbstractIteratorBinary>),
    AbstractNumeric(AbstractIterator<AbstractIteratorNumeric>),
    SingleValue(SingleValueDocValuesFieldUpdatesIterator),
}

impl DocValuesIterator for DocValuesFieldIteratorEnum {
    fn advance_exact(&mut self, target: i32) -> Result<bool> {
        match self {
            DocValuesFieldIteratorEnum::AbstractBinary(it) => it.advance_exact(target),
            DocValuesFieldIteratorEnum::AbstractNumeric(it) => it.advance_exact(target),
            DocValuesFieldIteratorEnum::SingleValue(it) => it.advance_exact(target),
        }
    }
}

impl DocIdSetIterator for DocValuesFieldIteratorEnum {
    fn doc_id(&self) -> i32 {
        match self {
            DocValuesFieldIteratorEnum::AbstractBinary(it) => it.doc_id(),
            DocValuesFieldIteratorEnum::AbstractNumeric(it) => it.doc_id(),
            DocValuesFieldIteratorEnum::SingleValue(it) => it.doc_id(),
        }
    }

    fn next_doc(&mut self) -> Result<i32> {
        match self {
            DocValuesFieldIteratorEnum::AbstractBinary(it) => it.next_doc(),
            DocValuesFieldIteratorEnum::AbstractNumeric(it) => it.next_doc(),
            DocValuesFieldIteratorEnum::SingleValue(it) => it.next_doc(),
        }
    }

    fn advance(&mut self, target: i32) -> Result<i32> {
        match self {
            DocValuesFieldIteratorEnum::AbstractBinary(it) => it.advance(target),
            DocValuesFieldIteratorEnum::AbstractNumeric(it) => it.advance(target),
            DocValuesFieldIteratorEnum::SingleValue(it) => it.slow_advance(target),
        }
    }

    fn slow_advance(&mut self, target: i32) -> Result<i32> {
        match self {
            DocValuesFieldIteratorEnum::AbstractBinary(it) => it.slow_advance(target),
            DocValuesFieldIteratorEnum::AbstractNumeric(it) => it.slow_advance(target),
            DocValuesFieldIteratorEnum::SingleValue(it) => it.slow_advance(target),
        }
    }

    fn cost(&self) -> Result<i64> {
        match self {
            DocValuesFieldIteratorEnum::AbstractBinary(it) => it.cost(),
            DocValuesFieldIteratorEnum::AbstractNumeric(it) => it.cost(),
            DocValuesFieldIteratorEnum::SingleValue(it) => it.cost(),
        }
    }
}

impl DocValuesFieldIterator for DocValuesFieldIteratorEnum {
    fn long_value(&mut self) -> Result<i64> {
        match self {
            DocValuesFieldIteratorEnum::AbstractBinary(_) => Err(LuceneError::illegal_state(
                "long_value is not supported for binary doc values",
            )),
            DocValuesFieldIteratorEnum::AbstractNumeric(it) => it.long_value(),
            DocValuesFieldIteratorEnum::SingleValue(it) => it.long_value(),
        }
    }

    fn binary_value(&mut self) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
        match self {
            DocValuesFieldIteratorEnum::AbstractBinary(it) => it.binary_value(),
            DocValuesFieldIteratorEnum::AbstractNumeric(it) => it.binary_value(),
            DocValuesFieldIteratorEnum::SingleValue(it) => it.binary_value(),
        }
    }

    fn del_gen(&self) -> i64 {
        match self {
            DocValuesFieldIteratorEnum::AbstractBinary(it) => it.del_gen(),
            DocValuesFieldIteratorEnum::AbstractNumeric(it) => it.del_gen(),
            DocValuesFieldIteratorEnum::SingleValue(it) => it.del_gen(),
        }
    }

    fn has_value(&self) -> bool {
        match self {
            DocValuesFieldIteratorEnum::AbstractBinary(it) => it.has_value(),
            DocValuesFieldIteratorEnum::AbstractNumeric(it) => it.has_value(),
            DocValuesFieldIteratorEnum::SingleValue(it) => it.has_value(),
        }
    }
}

/// Wraps the given iterator as a BinaryDocValues instance.
pub(crate) struct BinaryDocValuesDVFU<T>
where
    T: DocValuesFieldIterator,
{
    pub(crate) iterator: T,
}
impl<T> BinaryDocValuesDVFU<T>
where
    T: DocValuesFieldIterator,
{
    pub fn new(iterator: T) -> Self {
        Self { iterator }
    }
}

impl<T> DocValuesIterator for BinaryDocValuesDVFU<T>
where
    T: DocValuesFieldIterator,
{
    fn advance_exact(&mut self, target: i32) -> Result<bool> {
        self.iterator.advance_exact(target)
    }
}

impl<T> DocIdSetIterator for BinaryDocValuesDVFU<T>
where
    T: DocValuesFieldIterator,
{
    fn doc_id(&self) -> i32 {
        self.iterator.doc_id()
    }

    fn next_doc(&mut self) -> Result<i32> {
        self.iterator.next_doc()
    }

    fn advance(&mut self, target: i32) -> Result<i32> {
        self.iterator.advance(target)
    }

    fn cost(&self) -> Result<i64> {
        self.iterator.cost()
    }
}

impl<T> BinaryDocValues for BinaryDocValuesDVFU<T>
where
    T: DocValuesFieldIterator,
{
    fn binary_value(&mut self) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
        self.iterator.binary_value()
    }
}

/// Wraps the given iterator as a NumericDocValues instance.
pub(crate) struct NumericDocValuesDVFU<T>
where
    T: DocValuesFieldIterator,
{
    pub(crate) iterator: T,
}
impl<T> NumericDocValuesDVFU<T>
where
    T: DocValuesFieldIterator,
{
    pub fn new(iterator: T) -> Self {
        Self { iterator }
    }
}

impl<T> DocValuesIterator for NumericDocValuesDVFU<T>
where
    T: DocValuesFieldIterator,
{
    fn advance_exact(&mut self, target: i32) -> Result<bool> {
        self.iterator.advance_exact(target)
    }
}

impl<T> DocIdSetIterator for NumericDocValuesDVFU<T>
where
    T: DocValuesFieldIterator,
{
    fn doc_id(&self) -> i32 {
        self.iterator.doc_id()
    }

    fn next_doc(&mut self) -> Result<i32> {
        self.iterator.next_doc()
    }

    fn advance(&mut self, target: i32) -> Result<i32> {
        self.iterator.advance(target)
    }

    fn cost(&self) -> Result<i64> {
        self.iterator.cost()
    }
}

impl<T> NumericDocValues for NumericDocValuesDVFU<T>
where
    T: DocValuesFieldIterator,
{
    fn long_value(&mut self) -> Result<i64> {
        self.iterator.long_value()
    }
}

pub(crate) struct IteratorPQCmp;
impl IteratorPQCmp {
    pub fn new() -> Self {
        Self {}
    }
}
impl<T> Compare<T> for IteratorPQCmp
where
    T: DocValuesFieldIterator,
{
    fn less_than(&self, a: &T, b: &T) -> Result<bool> {
        // Sort by smaller doc_id
        let mut cmp = a.doc_id().cmp(&b.doc_id());
        if cmp == std::cmp::Ordering::Equal {
            // If doc_id is equal, sort by larger del_gen
            cmp = b.del_gen().cmp(&a.del_gen());
            // delGen values are unique across sub-iterators, so cmp should
            // never be equal
            assert_ne!(cmp, std::cmp::Ordering::Equal);
        }
        Ok(cmp == std::cmp::Ordering::Less)
    }
}

pub(crate) struct MergedIterator<T>
where
    T: DocValuesFieldIterator,
{
    queue: PriorityQueue<T, IteratorPQCmp>,
    doc: i32,
}
impl<T> MergedIterator<T>
where
    T: DocValuesFieldIterator,
{
    pub fn new(queue: PriorityQueue<T, IteratorPQCmp>) -> Result<Self> {
        Ok(Self { queue, doc: -1 })
    }
}

impl<T> DocValuesIterator for MergedIterator<T> where T: DocValuesFieldIterator {}

impl<T> DocValuesFieldIterator for MergedIterator<T>
where
    T: DocValuesFieldIterator,
{
    fn long_value(&mut self) -> Result<i64> {
        self.queue
            .top_mut()
            .expect("priority queue top element should exist")
            .long_value()
    }

    fn binary_value(&mut self) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
        self.queue
            .top_mut()
            .expect("priority queue top element should exist")
            .binary_value()
    }

    fn del_gen(&self) -> i64 {
        unreachable!("del_gen is not supported")
    }

    fn has_value(&self) -> bool {
        self.queue.top().map(|top| top.has_value()).unwrap_or(false)
    }
}
impl<T> DocIdSetIterator for MergedIterator<T>
where
    T: DocValuesFieldIterator,
{
    fn doc_id(&self) -> i32 {
        self.doc
    }

    fn next_doc(&mut self) -> Result<i32> {
        loop {
            if self.queue.size() == 0 {
                self.doc = NO_MORE_DOCS;
                break;
            }
            let new_doc = self
                .queue
                .top()
                .expect("priority queue top element should exist")
                .doc_id();

            if new_doc != self.doc {
                // Ensure the new document ID is greater than the current
                // document ID
                debug_assert!(new_doc > self.doc, "doc={} new_doc={}", self.doc, new_doc);
                self.doc = new_doc;
                break;
            }

            if self
                .queue
                .top_mut()
                .expect("priority queue top element should exist")
                .next_doc()?
                == NO_MORE_DOCS
            {
                self.queue.pop()?;
            } else {
                self.queue.update_top()?;
            }
        }
        Ok(self.doc)
    }
}
pub(crate) struct AbstractIterator<A>
where
    A: AbstractIteratorBase,
{
    inner: DocValuesFieldInnerIter,
    idx: i64,
    doc: i32,
    del_gen: i64,
    has_value: bool,
    sub: A,
}

impl<A> AbstractIterator<A>
where
    A: AbstractIteratorBase,
{
    pub fn new(inner: DocValuesFieldInnerIter, del_gen: i64, sub: A) -> Self {
        AbstractIterator {
            inner,
            idx: 0,
            doc: -1,
            del_gen,
            has_value: false,
            sub,
        }
    }
}

impl<A> DocValuesIterator for AbstractIterator<A> where A: AbstractIteratorBase {}

impl<A> DocIdSetIterator for AbstractIterator<A>
where
    A: AbstractIteratorBase,
{
    fn doc_id(&self) -> i32 {
        self.doc
    }

    fn next_doc(&mut self) -> Result<i32> {
        if self.idx >= self.inner.size as i64 {
            self.doc = NO_MORE_DOCS;
            return Ok(self.doc);
        }
        let mut long_doc = self.inner.docs.get_immutable(self.idx)?;
        self.idx += 1;

        while self.idx < self.inner.size as i64 {
            // Scan forward to last update to this doc
            let next_long_doc = self.inner.docs.get_immutable(self.idx)?;
            if (long_doc as u64 >> 1) != (next_long_doc as u64 >> 1) {
                break;
            }
            long_doc = next_long_doc;
            self.idx += 1;
        }

        self.has_value = (long_doc & HAS_VALUE_MASK) > 0;
        if self.has_value {
            self.sub.set(self.idx - 1)?;
        }
        debug_assert!((long_doc as u64 >> SHIFT) <= i32::MAX as u64);
        self.doc = (long_doc as u64 >> SHIFT) as i32;
        Ok(self.doc)
    }
}

impl<A> DocValuesFieldIterator for AbstractIterator<A>
where
    A: AbstractIteratorBase,
{
    fn long_value(&mut self) -> Result<i64> {
        self.sub.long_value()
    }

    fn binary_value(&mut self) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
        self.sub.binary_value()
    }

    fn del_gen(&self) -> i64 {
        self.del_gen
    }

    fn has_value(&self) -> bool {
        self.has_value
    }
}
pub trait AbstractIteratorBase {
    /// Called when the iterator moves to the next document.
    ///
    /// # Arguments
    ///
    /// * `idx` - The internal index to set the value to.
    fn set(&mut self, idx: i64) -> Result<()>;
    fn long_value(&mut self) -> Result<i64>;
    fn binary_value(&mut self) -> Result<Cow<'_, BytesRef<Vec<u8>>>>;
}

pub(crate) struct SingleValueDocValuesFieldUpdates {
    sub_update: Arc<SingleValueNumericDocValuesFieldUpdates>,
    bit_set: SparseFixedBitSet,
    has_no_value: Option<SparseFixedBitSet>,
    max_doc: i32,
    del_gen: i64,
    has_at_least_one_value: bool,
    lock: Mutex<()>,
    dov_values_type: DocValuesType,

    // for reused iterators
    bit_set_iter: Option<Arc<SparseFixedBitSet>>,
    has_no_value_iter: Option<Arc<SparseFixedBitSet>>,
}

impl SingleValueDocValuesFieldUpdates {
    pub fn new(
        sub: SingleValueNumericDocValuesFieldUpdates,
        max_doc: i32,
        del_gen: i64,
        dov_values_type: DocValuesType,
    ) -> Result<Self> {
        Ok(Self {
            sub_update: Arc::new(sub),
            bit_set: SparseFixedBitSet::new(max_doc)?,
            has_no_value: None,
            max_doc,
            del_gen,
            has_at_least_one_value: false,
            lock: Mutex::new(()),
            dov_values_type,
            bit_set_iter: None,
            has_no_value_iter: None,
        })
    }
    pub fn binary_value(&self) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
        self.sub_update.binary_value()
    }
    pub fn long_value(&self) -> Result<i64> {
        self.sub_update.long_value()
    }
}

impl Accountable for SingleValueDocValuesFieldUpdates {
    fn ram_bytes_used(&self) -> Result<i64> {
        todo!()
    }
}

impl DocValuesFieldUpdatesBase for SingleValueDocValuesFieldUpdates {
    fn finish(&mut self) {
        self.bit_set_iter = Some(Arc::new(std::mem::take(&mut self.bit_set)));
        if let Some(has_no_value) = self.has_no_value.take() {
            self.has_no_value_iter = Some(Arc::new(has_no_value));
        }
    }

    fn add_value(&mut self, doc: i32, value: i64, _index: i32) -> Result<()> {
        debug_assert!(self.sub_update.long_value()? == value);
        self.bit_set.set(doc);

        self.has_at_least_one_value = true;
        if self.has_no_value.is_some() {
            self.has_no_value.as_mut().unwrap().clear_with_index(doc);
        }
        Ok(())
    }

    fn add_byte_ref(&mut self, doc: i32, value: &BytesRef<Vec<u8>>, _index: i32) -> Result<()> {
        debug_assert!(self.sub_update.binary_value()?.as_ref() == value);
        self.bit_set.set(doc);
        self.has_at_least_one_value = true;
        if self.has_no_value.is_some() {
            self.has_no_value.as_mut().unwrap().clear_with_index(doc);
        }
        Ok(())
    }

    fn add_iterator<T: DocValuesFieldIterator>(
        &mut self,
        _doc_id: i32,
        _iterator: &mut T,
    ) -> Result<()> {
        unreachable!("add_iterator is not supported")
    }

    fn iterator(
        &self,
        _inner: DocValuesFieldInnerIter,
        _del_gen: i64,
    ) -> Result<DocValuesFieldIteratorEnum> {
        let iterator = BitSetIterator::new(
            self.bit_set_iter.as_ref().unwrap().clone(),
            self.max_doc as i64,
        )?;
        Ok(DocValuesFieldIteratorEnum::SingleValue(
            SingleValueDocValuesFieldUpdatesIterator::new(
                iterator,
                self.del_gen,
                self.has_no_value_iter.clone(),
                self.sub_update.clone(),
            )?,
        ))
    }

    fn reset(&mut self, doc: i32) -> Result<()> {
        let _guide = self.lock.lock();
        self.bit_set.set(doc);
        self.has_at_least_one_value = true;
        if self.has_no_value.is_none() {
            self.has_no_value = Some(SparseFixedBitSet::new(self.max_doc)?);
        }
        self.has_no_value.as_mut().unwrap().set(doc);
        drop(_guide);
        Ok(())
    }

    fn need_reset(&self) -> bool {
        true
    }

    fn any(&self, super_any: bool) -> bool {
        let _guide = self.lock.lock();
        let v = super_any || self.has_at_least_one_value;
        drop(_guide);
        v
    }

    fn need_any(&self) -> bool {
        true
    }

    fn sub_type(&self) -> DocValuesType {
        self.dov_values_type
    }

    fn need_add_doc(&self) -> bool {
        false
    }
}

pub trait SingleValueDocValuesFieldUpdatesBase {
    fn binary_value(&self) -> Result<Cow<'_, BytesRef<Vec<u8>>>>;
    fn long_value(&self) -> Result<i64>;
    fn sub_type(&self) -> DocValuesType;
}
pub struct SingleValueDocValuesFieldUpdatesIterator {
    del_gen: i64,
    has_no_value: Option<Arc<SparseFixedBitSet>>,
    iterator: BitSetIterator<SparseFixedBitSet, Arc<SparseFixedBitSet>>,
    single: Arc<SingleValueNumericDocValuesFieldUpdates>,
}
impl SingleValueDocValuesFieldUpdatesIterator {
    /// Creates a new instance of `SingleValueDocValuesFieldUpdatesIterator`.
    ///
    /// # Note
    /// Avoid using the `Default` trait. This constructor should be used
    /// instead.
    pub fn new(
        iterator: BitSetIterator<SparseFixedBitSet, Arc<SparseFixedBitSet>>,
        del_gen: i64,
        has_no_value: Option<Arc<SparseFixedBitSet>>,
        single: Arc<SingleValueNumericDocValuesFieldUpdates>,
    ) -> Result<Self> {
        Ok(Self {
            del_gen,
            has_no_value,
            iterator,
            single,
        })
    }
}

impl DocValuesIterator for SingleValueDocValuesFieldUpdatesIterator {}

impl DocValuesFieldIterator for SingleValueDocValuesFieldUpdatesIterator {
    fn long_value(&mut self) -> Result<i64> {
        self.single.long_value()
    }

    fn binary_value(&mut self) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
        self.single.binary_value()
    }

    fn del_gen(&self) -> i64 {
        self.del_gen
    }

    fn has_value(&self) -> bool {
        if self.has_no_value.is_some() {
            !self
                .has_no_value
                .as_ref()
                .unwrap()
                .get(self.iterator.doc_id())
        } else {
            true
        }
    }
}
impl DocIdSetIterator for SingleValueDocValuesFieldUpdatesIterator {
    fn doc_id(&self) -> i32 {
        self.iterator.doc_id()
    }

    fn next_doc(&mut self) -> Result<i32> {
        self.iterator.next_doc()
    }
}

pub enum DocValuesFieldUpdatesEnum {
    Numeric(DocValuesFieldUpdates<NumericDocValuesFieldUpdates>),
    Binary(DocValuesFieldUpdates<BinaryDocValuesFieldUpdates>),
    SingleValue(DocValuesFieldUpdates<SingleValueDocValuesFieldUpdates>),
}
impl DocValuesFieldUpdatesEnum {
    pub(crate) fn tp(&self) -> &DocValuesType {
        match self {
            DocValuesFieldUpdatesEnum::Numeric(u) => &u.doc_values_type,
            DocValuesFieldUpdatesEnum::Binary(u) => &u.doc_values_type,
            DocValuesFieldUpdatesEnum::SingleValue(u) => &u.doc_values_type,
        }
    }
    pub(crate) fn field(&self) -> &str {
        match self {
            DocValuesFieldUpdatesEnum::Numeric(u) => &u.field,
            DocValuesFieldUpdatesEnum::Binary(u) => &u.field,
            DocValuesFieldUpdatesEnum::SingleValue(u) => &u.field,
        }
    }

    pub(crate) fn get_finished(&self) -> Result<bool> {
        match self {
            DocValuesFieldUpdatesEnum::Numeric(u) => u.get_finished(),
            DocValuesFieldUpdatesEnum::Binary(u) => u.get_finished(),
            DocValuesFieldUpdatesEnum::SingleValue(u) => u.get_finished(),
        }
    }

    pub(crate) fn ram_bytes_used(&self) -> Result<i64> {
        match self {
            DocValuesFieldUpdatesEnum::Numeric(u) => u.ram_bytes_used(),
            DocValuesFieldUpdatesEnum::Binary(u) => u.ram_bytes_used(),
            DocValuesFieldUpdatesEnum::SingleValue(u) => u.ram_bytes_used(),
        }
    }

    pub(crate) fn del_gen(&self) -> i64 {
        match self {
            DocValuesFieldUpdatesEnum::Numeric(u) => u.del_gen,
            DocValuesFieldUpdatesEnum::Binary(u) => u.del_gen,
            DocValuesFieldUpdatesEnum::SingleValue(u) => u.del_gen,
        }
    }
    pub(crate) fn iterator(&self) -> Result<DocValuesFieldIteratorEnum> {
        match self {
            DocValuesFieldUpdatesEnum::Numeric(u) => u.iterator(),
            DocValuesFieldUpdatesEnum::Binary(u) => u.iterator(),
            DocValuesFieldUpdatesEnum::SingleValue(u) => u.iterator(),
        }
    }
    pub(crate) fn any(&self) -> bool {
        match self {
            DocValuesFieldUpdatesEnum::Numeric(u) => u.any(),
            DocValuesFieldUpdatesEnum::Binary(u) => u.any(),
            DocValuesFieldUpdatesEnum::SingleValue(u) => u.any(),
        }
    }
    pub(crate) fn get_type(&self) -> &DocValuesType {
        match self {
            DocValuesFieldUpdatesEnum::Numeric(u) => &u.doc_values_type,
            DocValuesFieldUpdatesEnum::Binary(u) => &u.doc_values_type,
            DocValuesFieldUpdatesEnum::SingleValue(u) => &u.doc_values_type,
        }
    }
    pub(crate) fn reset(&mut self, doc: i32) -> Result<()> {
        match self {
            DocValuesFieldUpdatesEnum::Numeric(u) => u.reset(doc),
            DocValuesFieldUpdatesEnum::Binary(u) => u.reset(doc),
            DocValuesFieldUpdatesEnum::SingleValue(u) => u.reset(doc),
        }
    }
    pub(crate) fn add_value(&mut self, doc: i32, value: i64) -> Result<()> {
        match self {
            DocValuesFieldUpdatesEnum::Numeric(u) => u.add_value(doc, value),
            DocValuesFieldUpdatesEnum::Binary(u) => u.add_value(doc, value),
            DocValuesFieldUpdatesEnum::SingleValue(u) => u.add_value(doc, value),
        }
    }
    pub(crate) fn add_binary_value(&mut self, doc: i32, value: &BytesRef<Vec<u8>>) -> Result<()> {
        match self {
            DocValuesFieldUpdatesEnum::Numeric(u) => u.add_byte_ref(doc, value),
            DocValuesFieldUpdatesEnum::Binary(u) => u.add_byte_ref(doc, value),
            DocValuesFieldUpdatesEnum::SingleValue(u) => u.add_byte_ref(doc, value),
        }
    }
    pub(crate) fn finish(&mut self) -> Result<()> {
        match self {
            DocValuesFieldUpdatesEnum::Numeric(u) => u.finish(),
            DocValuesFieldUpdatesEnum::Binary(u) => u.finish(),
            DocValuesFieldUpdatesEnum::SingleValue(u) => u.finish(),
        }
    }
}
pub(crate) const PAGE_SIZE: i32 = 1024;
const HAS_VALUE_MASK: i64 = 1;
const HAS_NO_VALUE_MASK: i64 = 0;
// we use the first bit of each value to mark if the doc has a value or not
const SHIFT: i32 = 1;
pub fn merged_iterator<T>(subs: Vec<T>) -> Result<Option<MergedIterator<T>>>
where
    T: DocValuesFieldIterator,
{
    // Due to the characteristics of the Rust language, to reduce complexity,
    // we add the element to the queue for processing even if there is only one
    // element. if subs.len() == 1 {
    //
    // }

    // Priority queue to sort iterators by doc_id and del_gen
    let mut queue = PriorityQueue::new(subs.len() as i32, IteratorPQCmp::new())?;

    for mut sub in subs {
        if sub.next_doc()? != NO_MORE_DOCS {
            queue.add(sub)?;
        }
    }

    if queue.size() == 0 {
        return Ok(None);
    }
    let value = MergedIterator::new(queue)?;
    Ok(Some(value))
}
/// Wraps the given iterator as a BinaryDocValues instance.
fn get_binary_doc_values<T: DocValuesFieldIterator>(iterator: T) {
    BinaryDocValuesDVFU::new(iterator);
}
/// Wraps the given iterator as a NumericDocValues instance.
fn get_numeric_doc_values<T: DocValuesFieldIterator>(iterator: T) {
    NumericDocValuesDVFU::new(iterator);
}

#[cfg(test)]
mod tests {
    use rand::Rng;
    use rand::prelude::SliceRandom;

    use crate::core::index::doc_values_field_updates::merged_iterator;
    use crate::core::index::doc_values_field_updates::{
        DocValuesFieldIterator, DocValuesFieldUpdates, DocValuesFieldUpdatesBase,
        SingleValueDocValuesFieldUpdates, SingleValueDocValuesFieldUpdatesBase,
    };
    use crate::core::index::numeric_doc_values_field_updates::{
        NumericDocValuesFieldUpdates, SingleValueNumericDocValuesFieldUpdates,
    };
    use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
    use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
    use crate::core::util::error::lucene_error::Result;
    use crate::test::util::lucene_test_case::lucene_test_case_util::{random, rarely};

    #[allow(dead_code)] // for quick search
    pub struct TestDocValuesFieldUpdates;
    #[test]
    fn test_merge_iterator() -> Result<()> {
        let mut random = random();
        let sub_update1 = NumericDocValuesFieldUpdates::new()?;
        let mut updates1 =
            DocValuesFieldUpdates::new(6, 0, "test", sub_update1.sub_type(), sub_update1)?;
        let sub_update2 = NumericDocValuesFieldUpdates::new()?;
        let mut updates2 =
            DocValuesFieldUpdates::new(6, 1, "test", sub_update2.sub_type(), sub_update2)?;
        let sub_update3 = NumericDocValuesFieldUpdates::new()?;
        let mut updates3 =
            DocValuesFieldUpdates::new(6, 2, "test", sub_update3.sub_type(), sub_update3)?;
        let sub_update4 = NumericDocValuesFieldUpdates::new()?;
        let mut updates4 =
            DocValuesFieldUpdates::new(6, 3, "test", sub_update4.sub_type(), sub_update4)?;

        updates1.add_value(0, 1)?;
        updates1.add_value(4, 0)?;
        updates1.add_value(1, 4)?;
        updates1.add_value(2, 5)?;
        updates1.add_value(4, 9)?;
        assert!(updates1.any());

        updates2.add_value(0, 18)?;
        updates2.add_value(1, 7)?;
        updates2.add_value(2, 19)?;
        updates2.add_value(5, 24)?;
        assert!(updates2.any());

        updates3.add_value(2, 42)?;
        assert!(updates3.any());
        assert!(!updates4.any());

        // Finish updates
        updates1.finish()?;
        updates2.finish()?;
        updates3.finish()?;
        updates4.finish()?;

        // Create iterators
        let mut iterators = vec![
            updates1.iterator()?,
            updates2.iterator()?,
            updates3.iterator()?,
            updates4.iterator()?,
        ];

        // Shuffle iterators (simulate randomness)
        iterators.shuffle(&mut random);

        // Merge iterators
        let merged_iterator_result = merged_iterator(iterators)?;
        assert!(merged_iterator_result.is_some());
        let mut merged_iterator = merged_iterator_result.unwrap();

        // Verify merged iterator results
        assert_eq!(merged_iterator.next_doc()?, 0);
        assert_eq!(merged_iterator.long_value()?, 18);

        assert_eq!(merged_iterator.next_doc()?, 1);
        assert_eq!(merged_iterator.long_value()?, 7);

        assert_eq!(merged_iterator.next_doc()?, 2);
        assert_eq!(merged_iterator.long_value()?, 42);

        assert_eq!(merged_iterator.next_doc()?, 4);
        assert_eq!(merged_iterator.long_value()?, 9);

        assert_eq!(merged_iterator.next_doc()?, 5);
        assert_eq!(merged_iterator.long_value()?, 24);

        assert_eq!(merged_iterator.next_doc()?, NO_MORE_DOCS);
        Ok(())
    }
    #[test]
    fn test_update_and_reset_same_doc() -> Result<()> {
        let sub_update = NumericDocValuesFieldUpdates::new()?;
        let mut updates =
            DocValuesFieldUpdates::new(2, 0, "test", sub_update.sub_type(), sub_update)?;

        updates.add_value(0, 1)?;
        updates.reset(0)?;
        updates.finish()?;

        let mut iterator = updates.iterator()?;
        assert_eq!(iterator.next_doc()?, 0);
        assert!(!iterator.has_value());
        assert_eq!(iterator.next_doc()?, NO_MORE_DOCS);

        Ok(())
    }
    #[test]
    fn test_update_and_reset_update_same_doc() -> Result<()> {
        let sub_update = NumericDocValuesFieldUpdates::new()?;
        let mut updates =
            DocValuesFieldUpdates::new(3, 0, "test", sub_update.sub_type(), sub_update)?;

        updates.add_value(0, 1)?;
        updates.reset(0)?;
        updates.add_value(0, 2)?;
        updates.finish()?;

        let mut iterator = updates.iterator()?;
        assert_eq!(iterator.next_doc()?, 0);
        assert!(iterator.has_value());
        assert_eq!(iterator.long_value()?, 2);
        assert_eq!(iterator.next_doc()?, NO_MORE_DOCS);

        Ok(())
    }
    #[test]
    fn test_updates_and_reset_random() -> Result<()> {
        let mut random = random();

        let sub_update = NumericDocValuesFieldUpdates::new()?;
        let mut updates =
            DocValuesFieldUpdates::new(10, 0, "test", sub_update.sub_type(), sub_update)?;

        let num_updates = 10 + random.random_range(0..100);
        let mut values: [Option<i32>; 5] = [None; 5];

        for (i, value) in values.iter_mut().enumerate() {
            if random.random_bool(0.5) {
                *value = None;
                updates.reset(i as i32)?;
            } else {
                let val = random.random_range(0..100);
                *value = Some(val);
                updates.add_value(i as i32, val as i64)?;
            }
        }

        for _ in 0..num_updates {
            let doc_id = random.random_range(0..5);
            if random.random_bool(0.5) {
                values[doc_id] = None;
                updates.reset(doc_id as i32)?;
            } else {
                let value = random.random_range(0..100);
                values[doc_id] = Some(value);
                updates.add_value(doc_id as i32, value as i64)?;
            }
        }

        updates.finish()?;

        // Test iterator could be reused multiple times
        let iter = random.random_range(0..2);
        for _ in 0..iter {
            let mut iterator = updates.iterator()?;
            let mut idx = 0;

            while iterator.next_doc()? != NO_MORE_DOCS {
                assert_eq!(idx, iterator.doc_id() as usize);
                if values[idx].is_none() {
                    assert!(!iterator.has_value());
                } else {
                    assert!(iterator.has_value());
                    assert_eq!(values[idx].unwrap() as i64, iterator.long_value()?);
                }
                idx += 1;
            }
        }

        Ok(())
    }
    #[test]
    fn test_shared_value_updates() -> Result<()> {
        let mut random = random();

        let del_gen = random.random::<i64>();
        let max_doc: i32 = 1 + random.random_range(0..1000);
        let value = random.random::<i64>();

        let sub_update1 = SingleValueNumericDocValuesFieldUpdates::new(value);
        let sub_type = sub_update1.sub_type();
        let sub_update2 =
            SingleValueDocValuesFieldUpdates::new(sub_update1, max_doc, del_gen, sub_type)?;
        let mut update =
            DocValuesFieldUpdates::new(max_doc, del_gen, "foo", sub_type, sub_update2)?;

        assert_eq!(value, update.sub_update.long_value()?);

        let mut values: Vec<Option<bool>> = vec![None; max_doc as usize];
        let mut any = false;
        let no_reset = random.random_bool(0.5);

        for (i, tmp_value) in values.iter_mut().enumerate() {
            if random.random_bool(0.5) {
                *tmp_value = Some(true);
                any = true;
                update.add_value(i as i32, value)?;
            } else if random.random_bool(0.5) && !no_reset {
                *tmp_value = None;
                any = true;
                update.reset(i as i32)?;
            } else {
                *tmp_value = Some(false);
            }
        }

        if !no_reset {
            for (i, tmp_value) in values.iter_mut().enumerate() {
                if rarely(&mut random) {
                    if tmp_value.is_none() {
                        *tmp_value = Some(true);
                        update.add_value(i as i32, value)?;
                    } else if *tmp_value == Some(true) {
                        *tmp_value = None;
                        update.reset(i as i32)?;
                    }
                }
            }
        }

        update.finish()?;
        assert_eq!(any, update.any());
        let mut iterator = update.iterator()?;
        assert_eq!(del_gen, iterator.del_gen());

        let mut index = 0;

        while iterator.next_doc()? != NO_MORE_DOCS {
            let doc = iterator.doc_id() as usize;

            if index < doc {
                values[index..doc]
                    .iter()
                    .for_each(|value| assert_eq!(*value, Some(false)));
                index = doc;
            }

            if index == doc {
                if values[index].is_none() {
                    assert!(!iterator.has_value());
                } else {
                    assert!(iterator.has_value());
                    assert_eq!(value, iterator.long_value()?);
                }
                index += 1;
            }
        }

        Ok(())
    }
}
