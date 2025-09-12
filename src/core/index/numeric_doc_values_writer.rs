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
use crate::core::codecs::dummy::dummy_binary_doc_values::DummyBinaryDocValues;
use crate::core::codecs::dummy::dummy_doc_values_skipper::DummyDocValuesSkipper;
use crate::core::codecs::dummy::dummy_sorted_doc_values::DummySortedDocValues;
use crate::core::codecs::dummy::dummy_sorted_numeric_doc_values::DummySortedNumericDocValues;
use crate::core::codecs::dummy::dummy_sorted_set_doc_values::DummySortedSetDocValues;
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::doc_values_writer::DocValuesWriter;
use crate::core::index::docs_with_field_set::{DocsWithFieldSet, DocsWithFieldSetDISI};
use crate::core::index::field_info::FieldInfo;
use crate::core::index::numeric_doc_values::Either2NumericDocValues;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::sorter::DocMap;
use crate::core::search::doc_id_set::DocIdSet;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::store::directory::Directory;
use crate::core::util::accountable::Accountable;
use crate::core::util::bit_set::BitSet;
use crate::core::util::error::lucene_error::LuceneError;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::packed::PackedInts;
use crate::core::util::packed::packed_long_values::{
    PackedLongValues, PackedLongValuesBuilder, PackedLongValuesIterator,
};
use crate::core::util::{CoreHelper, Counter, CounterEnumLock};
use std::cell::Cell;
use std::fmt::{Display, Formatter};
use std::rc::Rc;
use std::sync::Arc;

/// Buffers up pending long per doc, then flushes when segment flushes.
pub(crate) struct NumericDocValuesWriter {
    pending: PackedLongValuesBuilder,
    final_values: Option<PackedLongValues>,
    iw_bytes_used: CounterEnumLock,
    bytes_used: i64,
    docs_with_field: DocsWithFieldSet,
    field_info: Arc<FieldInfo>,
    last_doc_id: i32,
}

impl NumericDocValuesWriter {
    pub(crate) fn new(field_info: Arc<FieldInfo>, iw_bytes_used: CounterEnumLock) -> Result<Self> {
        let pending =
            PackedLongValues::delta_packed_long_values_builder_default(PackedInts::COMPACT)?;
        let docs_with_field = DocsWithFieldSet::new();
        let bytes_used = pending.ram_bytes_used()? + docs_with_field.ram_bytes_used()?;

        iw_bytes_used.lock().add_and_get(bytes_used);

        Ok(Self {
            pending,
            final_values: None,
            iw_bytes_used,
            bytes_used,
            docs_with_field,
            field_info,
            last_doc_id: -1,
        })
    }

    pub(crate) fn add_value(&mut self, doc_id: i32, value: i64) -> Result<()> {
        if doc_id <= self.last_doc_id {
            return Err(LuceneError::illegal_argument(format!(
                "DocValuesField \"{}\" appears more than once in this document (only one value is allowed per field)",
                self.field_info.name
            )));
        }

        self.pending.add(value)?;
        self.docs_with_field.add(doc_id)?;
        self.update_bytes_used()?;
        self.last_doc_id = doc_id;
        Ok(())
    }

    fn update_bytes_used(&mut self) -> Result<()> {
        let new_bytes_used =
            self.pending.ram_bytes_used()? + self.docs_with_field.ram_bytes_used()?;
        self.iw_bytes_used
            .lock()
            .add_and_get(new_bytes_used - self.bytes_used);
        self.bytes_used = new_bytes_used;
        Ok(())
    }
}

impl Display for NumericDocValuesWriter {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", std::any::type_name::<Self>())
    }
}

impl DocValuesWriter for NumericDocValuesWriter {
    fn flush<D, DM, DC>(
        &mut self,
        sort_map: Option<Rc<DM>>,
        dv_consumer: &mut DC,
        _segment_info: &SegmentInfo<D>,
    ) -> Result<()>
    where
        D: Directory,
        DM: DocMap,
        DC: DocValuesConsumer,
    {
        // `final_values` should always not None here, because we call finish() before flush()
        // but we still keep the check here for consistent with Java Lucene.
        if self.final_values.is_none() {
            self.final_values = Some(std::mem::take(&mut self.pending).build()?)
        }
        let producer = get_doc_values_producer(
            self.field_info.clone(),
            self.final_values.as_ref().unwrap(),
            std::mem::take(&mut self.docs_with_field),
            sort_map,
        )?;
        dv_consumer.add_numeric_field(&self.field_info, &producer)?;
        Ok(())
    }

    type DocIdSetIterator = BufferedNumericDocValues;

    fn get_doc_values(&self) -> Result<Self::DocIdSetIterator> {
        if self.final_values.is_none() {
            return Err(LuceneError::illegal_state(
                "must be finished before getting doc values",
            ));
        }
        Ok(BufferedNumericDocValues::new(
            self.final_values.as_ref().unwrap(),
            self.docs_with_field.iterator()?.unwrap(),
        ))
    }

    fn finish(&mut self) -> Result<()> {
        self.docs_with_field.finish();
        if self.final_values.is_none() {
            self.final_values = Some(std::mem::take(&mut self.pending).build()?)
        }
        Ok(())
    }
}
pub(crate) struct DocValuesProducerImpl {
    sorted: Option<NumericDVs<FixedBitSet>>,
    docs_with_field: DocsWithFieldSet,
    values: PackedLongValues,
    writer_field_info: Arc<FieldInfo>,
}
impl DocValuesProducerImpl {
    pub(crate) fn new(
        sorted: Option<NumericDVs<FixedBitSet>>,
        docs_with_field: DocsWithFieldSet,
        values: PackedLongValues,
        writer_field_info: Arc<FieldInfo>,
    ) -> Self {
        Self {
            sorted,
            docs_with_field,
            values,
            writer_field_info,
        }
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
    type NumericDocValues =
        Either2NumericDocValues<BufferedNumericDocValues, SortingNumericDocValues<FixedBitSet>>;

    fn get_numeric(&self, field_info: &Arc<FieldInfo>) -> Result<Self::NumericDocValues> {
        if !Arc::ptr_eq(field_info, &self.writer_field_info) {
            return Err(LuceneError::illegal_argument("wrong fieldInfo"));
        }
        match self.sorted {
            Some(ref sorted) => Ok(Either2NumericDocValues::B(SortingNumericDocValues::new(
                sorted.clone(),
            ))),
            None => Ok(Either2NumericDocValues::A(BufferedNumericDocValues::new(
                &self.values,
                self.docs_with_field.iterator()?.unwrap(),
            ))),
        }
    }

    type BinaryDocValues = DummyBinaryDocValues;
    type SortedDocValues = DummySortedDocValues;
    type SortedNumericDocValues = DummySortedNumericDocValues;
    type SortedSetDocValues = DummySortedSetDocValues;
    type DocValuesSkipper = DummyDocValuesSkipper;
}

// iterates over the values we have in ram
pub(crate) struct BufferedNumericDocValues {
    iter: PackedLongValuesIterator,
    doc_with_field: DocsWithFieldSetDISI,
    value: i64,
}
impl BufferedNumericDocValues {
    pub(crate) fn new(values: &PackedLongValues, doc_with_field: DocsWithFieldSetDISI) -> Self {
        Self {
            iter: values.iterator(),
            doc_with_field,
            value: 0,
        }
    }
}

impl DocValuesIterator for BufferedNumericDocValues {}

impl DocIdSetIterator for BufferedNumericDocValues {
    fn doc_id(&self) -> i32 {
        self.doc_with_field.doc_id()
    }

    fn next_doc(&mut self) -> Result<i32> {
        let doc_id = self.doc_with_field.next_doc()?;
        if doc_id != NO_MORE_DOCS {
            self.value = self.iter.next_value();
        }
        Ok(doc_id)
    }

    fn advance(&mut self, _target: i32) -> Result<i32> {
        Err(LuceneError::unsupported_operation(""))
    }

    fn cost(&self) -> Result<i64> {
        self.doc_with_field.cost()
    }
}

impl NumericDocValues for BufferedNumericDocValues {
    fn long_value(&mut self) -> Result<i64> {
        Ok(self.value)
    }
}

pub(crate) struct SortingNumericDocValues<T>
where
    T: BitSet,
{
    dvs: NumericDVs<T>,
    doc_id: i32,
    cost: Cell<i64>,
}

impl<T> SortingNumericDocValues<T>
where
    T: BitSet,
{
    pub(crate) fn new(dvs: NumericDVs<T>) -> Self {
        Self {
            dvs,
            doc_id: -1,
            cost: Cell::new(-1),
        }
    }
}

impl<T> DocValuesIterator for SortingNumericDocValues<T> where T: BitSet {}

impl<T> DocIdSetIterator for SortingNumericDocValues<T>
where
    T: BitSet,
{
    fn doc_id(&self) -> i32 {
        self.doc_id
    }

    fn next_doc(&mut self) -> Result<i32> {
        if self.doc_id + 1 == self.dvs.max_doc() {
            self.doc_id = NO_MORE_DOCS;
        } else {
            self.doc_id = self.dvs.advance(self.doc_id + 1);
        }
        Ok(self.doc_id)
    }

    fn advance(&mut self, _target: i32) -> Result<i32> {
        Err(LuceneError::unsupported_operation("use nextDoc() instead"))
    }

    fn cost(&self) -> Result<i64> {
        if self.cost.get() == -1 {
            self.cost.set(self.dvs.cost());
        }
        Ok(self.cost.get())
    }
}

impl<T> NumericDocValues for SortingNumericDocValues<T>
where
    T: BitSet,
{
    fn long_value(&mut self) -> Result<i64> {
        Ok(self.dvs.values[self.doc_id as usize])
    }
}
#[derive(Clone)]
pub(crate) struct NumericDVs<T>
where
    T: BitSet,
{
    pub values: Rc<Vec<i64>>,
    pub docs_with_field: Option<Rc<T>>,
    pub max_doc: i32,
}

impl<T> NumericDVs<T>
where
    T: BitSet,
{
    pub fn new(values: Vec<i64>, docs_with_field: Option<T>) -> Self {
        debug_assert!(values.len() <= i32::MAX as usize);
        let docs_with_field = docs_with_field.map(Rc::new);
        let max_doc = values.len() as i32;
        Self {
            values: Rc::new(values),
            docs_with_field,
            max_doc,
        }
    }

    pub(crate) fn max_doc(&self) -> i32 {
        self.max_doc
    }

    fn advance_exact(&self, target: i32) -> bool {
        match &self.docs_with_field {
            Some(bits) => bits.get(target),
            None => true,
        }
    }
    pub(crate) fn advance(&self, target: i32) -> i32 {
        if let Some(bits) = &self.docs_with_field {
            bits.next_set_bit(target)
        } else {
            // Only called when target is less than maxDoc
            target
        }
    }
    pub(crate) fn cost(&self) -> i64 {
        match &self.docs_with_field {
            Some(bits) => bits.cardinality() as i64,
            None => self.max_doc as i64,
        }
    }
}

pub(crate) fn sort_doc_values<DV, M>(
    max_doc: i32,
    sort_map: &M,
    old_doc_values: &mut DV,
    dense: bool,
) -> Result<NumericDVs<FixedBitSet>>
where
    DV: NumericDocValues,
    M: DocMap,
{
    let mut docs_with_field = if !dense {
        Some(FixedBitSet::new(max_doc))
    } else {
        None
    };

    let mut values = vec![0i64; max_doc as usize];

    loop {
        let doc_id = old_doc_values.next_doc()?;
        if doc_id == NO_MORE_DOCS {
            break;
        }

        let new_doc_id = sort_map.old_to_new(doc_id);
        if let Some(bits) = &mut docs_with_field {
            bits.set(new_doc_id);
        }

        values[new_doc_id as usize] = old_doc_values.long_value()?;
    }
    Ok(NumericDVs::new(values, docs_with_field))
}

pub(crate) fn get_doc_values_producer<DM>(
    writer_field_info: Arc<FieldInfo>,
    values: &PackedLongValues,
    docs_with_field: DocsWithFieldSet,
    sort_map: Option<Rc<DM>>,
) -> Result<DocValuesProducerImpl>
where
    DM: DocMap,
{
    let sorter = if let Some(sort_map) = sort_map {
        let dense = sort_map.size() == docs_with_field.cardinality();
        let iter = match docs_with_field.iterator()? {
            Some(iter) => iter,
            None => return Err(LuceneError::illegal_state("DocsWithFieldSet is None")),
        };
        let mut old_values = BufferedNumericDocValues::new(values, iter);
        let sorted = sort_doc_values(sort_map.size(), sort_map.as_ref(), &mut old_values, dense)?;
        Some(sorted)
    } else {
        None
    };
    Ok(DocValuesProducerImpl::new(
        sorter,
        docs_with_field,
        values.clone(),
        writer_field_info,
    ))
}
