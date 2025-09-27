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
use crate::core::codecs::doc_values_consumer::DocValuesConsumer;
use crate::core::codecs::doc_values_producer::DocValuesProducer;
use crate::core::codecs::dummy::dummy_doc_values_skipper::DummyDocValuesSkipper;
use crate::core::codecs::dummy::dummy_numeric_doc_values::DummyNumericDocValues;
use crate::core::codecs::dummy::dummy_sorted_doc_values::DummySortedDocValues;
use crate::core::codecs::dummy::dummy_sorted_numeric_doc_values::DummySortedNumericDocValues;
use crate::core::codecs::dummy::dummy_sorted_set_doc_values::DummySortedSetDocValues;
use crate::core::index::binary_doc_values::BinaryDocValues;
use crate::core::index::binary_doc_values::Either2BinaryDocValues;
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::doc_values_writer::DocValuesWriter;
use crate::core::index::docs_with_field_set::{DocsWithFieldSet, DocsWithFieldSetDISI};
use crate::core::index::field_info::FieldInfo;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::sorter::DocMap;
use crate::core::index::{BytesRef, BytesRefBuilder};
use crate::core::search::doc_id_set::DocIdSet;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::store::directory::Directory;
use crate::core::store::{DataInput, DataOutput};
use crate::core::util::accountable::Accountable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::packed::PackedInts;
use crate::core::util::packed::packed_long_values::{
    PackedLongValues, PackedLongValuesBuilder, PackedLongValuesIterator,
};
use crate::core::util::paged_bytes::{
    PagedBytes, PagedBytesDataInput, PagedBytesDataOutput, get_data_input, get_data_output,
};
use crate::core::util::{
    BytesRefArray, CoreHelper, Counter, CounterEnum, CounterEnumBorrow, CounterEnumLock,
    SortableBytesRefArray,
};
use std::borrow::Cow;
use std::cell::RefCell;
use std::fmt::{Display, Formatter};
use std::rc::Rc;
use std::sync::Arc;

/// Buffers up pending `[u8]` per doc, then flushes when segment flushes.
pub(crate) struct BinaryDocValuesWriter {
    field_info: Arc<FieldInfo>,
    bytes_out: PagedBytesDataOutput,
    iw_bytes_used: CounterEnumLock,
    lengths: PackedLongValuesBuilder,
    docs_with_field: DocsWithFieldSet,
    bytes_used: i64,
    last_doc_id: i32,
    max_length: i32,
    final_lengths: Option<PackedLongValues>,
}

impl BinaryDocValuesWriter {
    pub(crate) fn new(field_info: Arc<FieldInfo>, iw_bytes_used: CounterEnumLock) -> Result<Self> {
        let bytes = PagedBytes::new(BLOCK_BITS);
        let bytes_out = get_data_output(bytes)?;
        let lengths =
            PackedLongValues::delta_packed_long_values_builder_default(PackedInts::COMPACT)?;
        let docs_with_field = DocsWithFieldSet::new();

        let bytes_used = lengths.ram_bytes_used()? + docs_with_field.ram_bytes_used()?;
        iw_bytes_used.lock().add_and_get(bytes_used);

        Ok(Self {
            field_info,
            bytes_out,
            iw_bytes_used,
            lengths,
            docs_with_field,
            bytes_used,
            last_doc_id: -1,
            max_length: 0,
            final_lengths: None,
        })
    }
    pub(crate) fn add_value(&mut self, doc_id: i32, value: &BytesRef<Vec<u8>>) -> Result<()> {
        if doc_id <= self.last_doc_id {
            return Err(LuceneError::illegal_argument(format!(
                "DocValuesField \"{}\" appears more than once in this document (only one value is allowed per field)",
                self.field_info.name
            )));
        }
        if value.length > MAX_LENGTH {
            return Err(LuceneError::illegal_argument(format!(
                "DocValuesField \"{}\" is too large, must be <= {}",
                self.field_info.name, MAX_LENGTH
            )));
        }

        self.max_length = self.max_length.max(value.length as i32);
        self.lengths.add(value.length as i64)?;

        self.bytes_out
            .write_bytes_range(&value.bytes, value.offset as i32, value.length as i32)?;

        self.docs_with_field.add(doc_id)?;
        self.update_bytes_used()?;

        self.last_doc_id = doc_id;
        Ok(())
    }

    fn update_bytes_used(&mut self) -> Result<()> {
        let new_bytes_used = self.lengths.ram_bytes_used()?
            + self.bytes_out.paged_bytes.ram_bytes_used()?
            + self.docs_with_field.ram_bytes_used()?;
        self.iw_bytes_used
            .lock()
            .add_and_get(new_bytes_used - self.bytes_used);
        self.bytes_used = new_bytes_used;
        Ok(())
    }
}

impl Display for BinaryDocValuesWriter {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", std::any::type_name::<Self>())
    }
}

impl DocValuesWriter for BinaryDocValuesWriter {
    fn flush<D, DM, DC>(
        &mut self,
        sort_map: Option<Arc<DM>>,
        dv_consumer: &mut DC,
        segment_info: &SegmentInfo<D>,
    ) -> Result<()>
    where
        D: Directory,
        DM: DocMap,
        DC: DocValuesConsumer,
    {
        self.bytes_out.paged_bytes.freeze(false)?;
        // final_lengths should always not None here, because we call finish() before flush()
        // but we still keep the check here for consistent with Java Lucene.
        if self.final_lengths.is_none() {
            self.final_lengths = Some(self.lengths.build()?);
        }
        let sorted = match sort_map {
            Some(sort_map) => {
                let mut buffered_binary_doc_values = BufferedBinaryDocValues::new(
                    self.final_lengths.as_ref().unwrap(),
                    self.max_length as usize,
                    get_data_input(&self.bytes_out.paged_bytes)?,
                    self.docs_with_field.iterator()?.unwrap(),
                );
                Some(BinaryDVs::new(
                    segment_info.max_doc()?,
                    sort_map.as_ref(),
                    &mut buffered_binary_doc_values,
                )?)
            },
            None => None,
        };

        let producer = DocValuesProducerImpl::new(
            self.field_info.clone(),
            self.final_lengths.take().unwrap(),
            self.max_length,
            std::mem::take(&mut self.bytes_out.paged_bytes),
            std::mem::take(&mut self.docs_with_field),
            sorted,
        )?;
        dv_consumer.add_binary_field(&self.field_info, &producer)
    }

    type DocIdSetIterator = BufferedBinaryDocValues<DocsWithFieldSetDISI, PagedBytesDataInput>;

    fn get_doc_values(&self) -> Result<Self::DocIdSetIterator> {
        if self.final_lengths.is_none() {
            return Err(LuceneError::illegal_state(
                "must be finished before getting doc values".to_string(),
            ));
        }
        Ok(BufferedBinaryDocValues::new(
            self.final_lengths.as_ref().unwrap(),
            self.max_length as usize,
            get_data_input(&self.bytes_out.paged_bytes)?,
            self.docs_with_field.iterator()?.unwrap(),
        ))
    }

    fn finish(&mut self) -> Result<()> {
        self.docs_with_field.finish();
        if self.final_lengths.is_none() {
            self.final_lengths = Some(self.lengths.build()?);
        }
        Ok(())
    }
}

pub(crate) struct DocValuesProducerImpl {
    field_info: Arc<FieldInfo>,
    final_lengths: PackedLongValues,
    max_length: i32,
    paged_bytes: PagedBytes,
    docs_with_field: DocsWithFieldSet,
    sorted: Option<BinaryDVs>,
}
impl DocValuesProducerImpl {
    pub(crate) fn new(
        field_info: Arc<FieldInfo>,
        final_lengths: PackedLongValues,
        max_length: i32,
        paged_bytes: PagedBytes,
        docs_with_field: DocsWithFieldSet,
        sorted: Option<BinaryDVs>,
    ) -> Result<Self> {
        Ok(Self {
            field_info,
            final_lengths,
            max_length,
            paged_bytes,
            docs_with_field,
            sorted,
        })
    }
}

impl Clone for DocValuesProducerImpl {
    fn clone(&self) -> Self {
        unreachable!(
            "{} {}",
            std::any::type_name::<Self>(),
            CoreHelper::CLONE_WARRING
        )
    }
}

impl DocValuesProducer for DocValuesProducerImpl {
    type NumericDocValues = DummyNumericDocValues;
    type BinaryDocValues = Either2BinaryDocValues<
        SortingBinaryDocValues,
        BufferedBinaryDocValues<DocsWithFieldSetDISI, PagedBytesDataInput>,
    >;

    fn get_binary(&self, field_info: &Arc<FieldInfo>) -> Result<Self::BinaryDocValues> {
        if Arc::ptr_eq(field_info, &self.field_info) {
            return Err(LuceneError::illegal_argument("wrong fieldInfo"));
        }
        match &self.sorted {
            Some(sorted) => Ok(Either2BinaryDocValues::A(SortingBinaryDocValues::new(
                sorted.clone(),
            ))),
            None => Ok(Either2BinaryDocValues::B(BufferedBinaryDocValues::new(
                &self.final_lengths,
                self.max_length as usize,
                get_data_input(&self.paged_bytes)?,
                self.docs_with_field.iterator()?.unwrap(),
            ))),
        }
    }

    type SortedDocValues = DummySortedDocValues;
    type SortedNumericDocValues = DummySortedNumericDocValues;
    type SortedSetDocValues = DummySortedSetDocValues;
    type DocValuesSkipper = DummyDocValuesSkipper;
}

// iterates over the values we have in ram
pub(crate) struct BufferedBinaryDocValues<D, DI>
where
    D: DocIdSetIterator,
    DI: DataInput,
{
    value: BytesRefBuilder<Vec<u8>>,
    lengths_iterator: PackedLongValuesIterator,
    docs_with_field: D,
    bytes_iter: DI,
}

impl<D, DI> BufferedBinaryDocValues<D, DI>
where
    D: DocIdSetIterator,
    DI: DataInput,
{
    pub(crate) fn new(
        lengths: &PackedLongValues,
        max_length: usize,
        bytes_iter: DI,
        docs_with_field: D,
    ) -> Self {
        let mut value = BytesRefBuilder::new();
        value.grow(max_length);
        Self {
            value,
            lengths_iterator: lengths.iterator(),
            docs_with_field,
            bytes_iter,
        }
    }
}

impl<D, DI> DocIdSetIterator for BufferedBinaryDocValues<D, DI>
where
    D: DocIdSetIterator,
    DI: DataInput,
{
    fn doc_id(&self) -> i32 {
        self.docs_with_field.doc_id()
    }

    fn next_doc(&mut self) -> Result<i32> {
        let doc_id = self.docs_with_field.next_doc()?;
        if doc_id != NO_MORE_DOCS {
            let length: i32 = self.lengths_iterator.next_value().try_into()?;
            self.value.set_length(length as usize);
            self.bytes_iter
                .read_bytes(&mut self.value.bytes_ref.bytes, 0, length)?;
        }
        Ok(doc_id)
    }

    fn advance(&mut self, _target: i32) -> Result<i32> {
        Err(LuceneError::unsupported_operation("use next_doc instead"))
    }

    fn cost(&self) -> Result<i64> {
        self.docs_with_field.cost()
    }
}

impl<D, DI> DocValuesIterator for BufferedBinaryDocValues<D, DI>
where
    D: DocIdSetIterator,
    DI: DataInput,
{
    fn advance_exact(&mut self, _target: i32) -> Result<bool> {
        Err(LuceneError::unsupported_operation("use next_doc instead"))
    }
}

impl<D, DI> BinaryDocValues for BufferedBinaryDocValues<D, DI>
where
    D: DocIdSetIterator,
    DI: DataInput,
{
    fn binary_value(&mut self) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
        Ok(Cow::Borrowed(self.value.get_bytes_ref()))
    }
}

pub(crate) struct SortingBinaryDocValues {
    dvs: BinaryDVs,
    spare: BytesRefBuilder<Vec<u8>>,
    doc_id: i32,
}

impl SortingBinaryDocValues {
    pub(crate) fn new(dvs: BinaryDVs) -> Self {
        Self {
            dvs,
            spare: BytesRefBuilder::new(),
            doc_id: -1,
        }
    }
}

impl DocIdSetIterator for SortingBinaryDocValues {
    fn doc_id(&self) -> i32 {
        self.doc_id
    }

    fn next_doc(&mut self) -> Result<i32> {
        loop {
            self.doc_id += 1;
            if self.doc_id as usize == self.dvs.offsets.len() {
                self.doc_id = NO_MORE_DOCS;
                break;
            }
            if self.dvs.offsets[self.doc_id as usize] > 0 {
                break;
            }
        }
        Ok(self.doc_id)
    }

    fn advance(&mut self, _target: i32) -> Result<i32> {
        Err(LuceneError::unsupported_operation("use next_doc instead"))
    }

    fn cost(&self) -> Result<i64> {
        Ok(self.dvs.values.size() as i64)
    }
}

impl DocValuesIterator for SortingBinaryDocValues {
    fn advance_exact(&mut self, _target: i32) -> Result<bool> {
        Err(LuceneError::unsupported_operation("use next_doc instead"))
    }
}

impl BinaryDocValues for SortingBinaryDocValues {
    fn binary_value(&mut self) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
        let idx = self.dvs.offsets[self.doc_id as usize] - 1;
        self.dvs.values.get(&mut self.spare, idx)?;
        Ok(Cow::Borrowed(self.spare.get_bytes_ref()))
    }
}

#[derive(Clone)]
pub(crate) struct BinaryDVs {
    pub(crate) offsets: Rc<Vec<i32>>,
    pub(crate) values: Rc<BytesRefArray<CounterEnumBorrow>>,
}

impl BinaryDVs {
    pub(crate) fn new<DM>(
        max_doc: i32,
        sort_map: &DM,
        old_values: &mut impl BinaryDocValues,
    ) -> Result<Self>
    where
        DM: DocMap,
    {
        let mut offsets = vec![0i32; max_doc as usize];
        let counter = Rc::new(RefCell::new(CounterEnum::new_counter(false)));
        let mut values = BytesRefArray::new(counter)?;
        let mut offset = 1i32; // 0 means no values for this document
        let mut doc_id;
        loop {
            doc_id = old_values.next_doc()?;
            if doc_id == NO_MORE_DOCS {
                break;
            }
            let new_doc = sort_map.old_to_new(doc_id) as usize;
            let val = old_values.binary_value()?;
            values.append(val.as_ref())?;
            offsets[new_doc] = offset;
            offset += 1;
        }
        Ok(BinaryDVs {
            offsets: Rc::new(offsets),
            values: Rc::new(values),
        })
    }
}

use crate::core::util::array_util::ArrayUtil;

// 4 kB block sizes for PagedBytes storage:
const BLOCK_BITS: usize = 12;
const MAX_LENGTH: usize = ArrayUtil::MAX_ARRAY_LENGTH;
