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
use crate::core::codecs::dummy::dummy_numeric_doc_values::DummyNumericDocValues;
use crate::core::codecs::dummy::dummy_sorted_doc_values::DummySortedDocValues;
use crate::core::codecs::dummy::dummy_sorted_set_doc_values::DummySortedSetDocValues;
use crate::core::index::doc_values::DocValues;
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::doc_values_writer::DocValuesWriter;
use crate::core::index::docs_with_field_set::{DocsWithFieldSet, DocsWithFieldSetDISI};
use crate::core::index::field_info::FieldInfo;
use crate::core::index::numeric_doc_values::Either2NumericDocValues;
use crate::core::index::numeric_doc_values_writer::{
    BufferedNumericDocValues, DocValuesProducerImpl, SortingNumericDocValues,
    get_doc_values_producer,
};
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::singleton_sorted_numeric_doc_values::SingletonSortedNumericDocValues;
use crate::core::index::sorted_numeric_doc_values::Either2SortedNumericDocValues;
use crate::core::index::sorted_numeric_doc_values::SortedNumericDocValues;
use crate::core::index::sorter::DocMap;
use crate::core::search::doc_id_set::DocIdSet;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::store::directory::Directory;
use crate::core::util::accountable::Accountable;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::long_values::LongValues as OtherLongValues;
use crate::core::util::packed::PackedInts;
use crate::core::util::packed::packed_long_values::{
    PackedLongValues, PackedLongValuesBuilder, PackedLongValuesIterator,
};
use crate::core::util::{CoreHelper, Counter, CounterEnumLock};
use std::fmt::{Display, Formatter};
use std::rc::Rc;
use std::sync::Arc;

/// Buffers up pending `[i64]` per doc, sorts, then flushes when segment flushes.
pub(crate) struct SortedNumericDocValuesWriter {
    pending: PackedLongValuesBuilder, // stream of all values
    pending_counts: Option<PackedLongValuesBuilder>, // count of values per doc
    docs_with_field: DocsWithFieldSet,
    iw_bytes_used: CounterEnumLock,
    bytes_used: i64, // this only tracks differences in 'pending' and 'pendingCounts'
    field_info: Arc<FieldInfo>,
    current_doc: i32,
    current_values: Vec<i64>,
    current_upto: usize,

    final_values: Option<PackedLongValues>,
    final_values_count: Option<PackedLongValues>,
}

impl SortedNumericDocValuesWriter {
    pub(crate) fn new(field_info: Arc<FieldInfo>, iw_bytes_used: CounterEnumLock) -> Result<Self> {
        let current_values = vec![0i64; 8];
        let docs_with_field = DocsWithFieldSet::new();
        let pending =
            PackedLongValues::delta_packed_long_values_builder_default(PackedInts::COMPACT)?;

        // TODO:  memory calculation not implemented
        let bytes_used = pending.ram_bytes_used()? + docs_with_field.ram_bytes_used()?;

        iw_bytes_used.lock().add_and_get(bytes_used);

        Ok(Self {
            pending,
            pending_counts: None,
            docs_with_field,
            iw_bytes_used,
            bytes_used,
            field_info,
            current_doc: -1,
            current_values,
            current_upto: 0,
            final_values: None,
            final_values_count: None,
        })
    }

    pub(crate) fn add_value(&mut self, doc_id: i32, value: i64) -> Result<()> {
        debug_assert!(doc_id >= self.current_doc);
        if doc_id != self.current_doc {
            self.finish_current_doc()?;
            self.current_doc = doc_id;
        }
        self.add_one_value(value)?;
        self.update_bytes_used()?;
        Ok(())
    }
    // finalize currentDoc: this sorts the values in the current doc
    fn finish_current_doc(&mut self) -> Result<()> {
        if self.current_doc == -1 {
            return Ok(());
        }
        if self.current_upto > 1 {
            self.current_values[..self.current_upto].sort_unstable();
        }
        for i in 0..self.current_upto {
            self.pending.add(self.current_values[i])?;
        }
        // record the number of values for this doc
        if let Some(pending_counts) = self.pending_counts.as_mut() {
            pending_counts.add(self.current_upto as i64)?;
        } else if self.current_upto != 1 {
            let mut pending_counts =
                PackedLongValues::delta_packed_long_values_builder_default(PackedInts::COMPACT)?;
            for _ in 0..self.docs_with_field.cardinality() {
                pending_counts.add(1)?;
            }
            pending_counts.add(self.current_upto as i64)?;
            self.pending_counts = Some(pending_counts);
        }
        self.current_upto = 0;
        self.docs_with_field.add(self.current_doc)?;
        Ok(())
    }

    fn add_one_value(&mut self, value: i64) -> Result<()> {
        if self.current_upto == self.current_values.len() {
            let len = self.current_values.len();
            ArrayUtil::grow_with_len(&mut self.current_values, len + 1);
        }
        self.current_values[self.current_upto] = value;
        self.current_upto += 1;
        Ok(())
    }

    fn update_bytes_used(&mut self) -> Result<()> {
        let pending_counts_usage = match &self.pending_counts {
            Some(c) => c.ram_bytes_used()?,
            None => 0,
        };
        // TODO: memory calculation not implemented
        let new_bytes_used = self.pending.ram_bytes_used()?
            + pending_counts_usage
            + self.docs_with_field.ram_bytes_used()?;

        self.iw_bytes_used
            .lock()
            .add_and_get(new_bytes_used - self.bytes_used);
        self.bytes_used = new_bytes_used;
        Ok(())
    }

    pub(crate) fn get_values(
        values: &PackedLongValues,
        value_counts: &Option<PackedLongValues>,
        docs_with_field: &DocsWithFieldSet,
    ) -> Result<
        Either2SortedNumericDocValues<
            SingletonSortedNumericDocValues<BufferedNumericDocValues>,
            BufferedSortedNumericDocValues<DocsWithFieldSetDISI>,
        >,
    > {
        let iter = docs_with_field
            .iterator()?
            .ok_or_else(|| LuceneError::illegal_state("docsWithField.iterator() returned None"))?;

        match value_counts {
            None => {
                let dv = BufferedNumericDocValues::new(values, iter);
                Ok(Either2SortedNumericDocValues::A(
                    DocValues::singleton_numeric(dv)?,
                ))
            },
            Some(value_counts) => {
                let dv = BufferedSortedNumericDocValues::new(values, value_counts, iter);
                Ok(Either2SortedNumericDocValues::B(dv))
            },
        }
    }
}

impl Display for SortedNumericDocValuesWriter {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", std::any::type_name::<Self>())
    }
}

impl DocValuesWriter for SortedNumericDocValuesWriter {
    fn flush<D, DM, DC>(
        &mut self,
        sort_map: Option<Rc<DM>>,
        dv_consumer: &mut DC,
        segment_info: &SegmentInfo<D>,
    ) -> Result<()>
    where
        D: Directory,
        DM: DocMap,
        DC: DocValuesConsumer,
    {
        // `final_values` should always not None here, because we call finish() before flush()
        // but we still keep the check here for consistent with Java Lucene.
        let (values, value_counts) = if self.final_values.is_none() {
            self.finish_current_doc()?;
            let values = self.pending.build()?;
            let value_counts = match &mut self.pending_counts {
                Some(p) => Some(p.build()?),
                None => None,
            };
            (values, value_counts)
        } else {
            (
                self.final_values.take().unwrap(),
                self.final_values_count.take(),
            )
        };
        if value_counts.is_none() {
            let single_value_producer = get_doc_values_producer(
                self.field_info.clone(),
                &values,
                std::mem::take(&mut self.docs_with_field),
                sort_map,
            )?;
            let producer = DocValuesProducerImpl1::new(single_value_producer)?;
            dv_consumer.add_sorted_numeric_field(&self.field_info, &producer)?;
            return Ok(());
        }

        let sorted = if let Some(sort_map) = sort_map {
            let mut v = SortedNumericDocValuesWriter::get_values(
                &values,
                &value_counts,
                &self.docs_with_field,
            )?;
            Some(LongValues::new(
                segment_info.max_doc()? as usize,
                &sort_map,
                &mut v,
                PackedInts::FASTEST,
            )?)
        } else {
            None
        };

        let producer = DocValuesProducerImpl2::new(
            self.field_info.clone(),
            std::mem::take(&mut self.docs_with_field),
            values,
            sorted,
            value_counts,
        )?;
        dv_consumer.add_sorted_numeric_field(&self.field_info, &producer)
    }

    type DocIdSetIterator = Either2SortedNumericDocValues<
        SingletonSortedNumericDocValues<BufferedNumericDocValues>,
        BufferedSortedNumericDocValues<DocsWithFieldSetDISI>,
    >;

    fn get_doc_values(&self) -> Result<Self::DocIdSetIterator> {
        if self.final_values.is_none() {
            return Err(LuceneError::illegal_state(
                "must be finished before getting doc values",
            ));
        }
        SortedNumericDocValuesWriter::get_values(
            self.final_values.as_ref().unwrap(),
            &self.final_values_count,
            &self.docs_with_field,
        )
    }

    fn finish(&mut self) -> Result<()> {
        self.docs_with_field.finish();
        if self.final_values.is_none() {
            debug_assert!(self.final_values_count.is_none());
            self.finish_current_doc()?;
            self.final_values = Option::from(self.pending.build()?);
            self.final_values_count = match &mut self.pending_counts {
                Some(p) => Some(p.build()?),
                None => None,
            };
        }
        Ok(())
    }
}
pub(crate) struct DocValuesProducerImpl1 {
    single_value_producer: DocValuesProducerImpl,
}
impl DocValuesProducerImpl1 {
    pub(crate) fn new(single_value_producer: DocValuesProducerImpl) -> Result<Self> {
        Ok(Self {
            single_value_producer,
        })
    }
}

impl Clone for DocValuesProducerImpl1 {
    fn clone(&self) -> Self {
        unreachable!(
            "{} {}",
            std::any::type_name::<Self>(),
            CoreHelper::CLONE_WARRING
        )
    }
}

impl DocValuesProducer for DocValuesProducerImpl1 {
    type NumericDocValues = DummyNumericDocValues;
    type BinaryDocValues = DummyBinaryDocValues;
    type SortedDocValues = DummySortedDocValues;
    type SortedNumericDocValues = SingletonSortedNumericDocValues<
        Either2NumericDocValues<BufferedNumericDocValues, SortingNumericDocValues<FixedBitSet>>,
    >;

    fn get_sorted_numeric(
        &self,
        field_info_in: &Arc<FieldInfo>,
    ) -> Result<Self::SortedNumericDocValues> {
        let v = self.single_value_producer.get_numeric(field_info_in)?;
        DocValues::singleton_numeric(v)
    }

    type SortedSetDocValues = DummySortedSetDocValues;
    type DocValuesSkipper = DummyDocValuesSkipper;
}

pub(crate) struct DocValuesProducerImpl2 {
    field_info: Arc<FieldInfo>,
    docs_with_field: DocsWithFieldSet,
    values: PackedLongValues,
    sorted: Option<LongValues>,
    value_counts: Option<PackedLongValues>,
}
impl DocValuesProducerImpl2 {
    fn new(
        field_info: Arc<FieldInfo>,
        docs_with_field: DocsWithFieldSet,
        values: PackedLongValues,
        sorted: Option<LongValues>,
        value_counts: Option<PackedLongValues>,
    ) -> Result<Self> {
        Ok(Self {
            field_info,
            docs_with_field,
            values,
            sorted,
            value_counts,
        })
    }
}

impl Clone for DocValuesProducerImpl2 {
    fn clone(&self) -> Self {
        unreachable!(
            "{} {}",
            std::any::type_name::<Self>(),
            CoreHelper::CLONE_WARRING
        )
    }
}

impl DocValuesProducer for DocValuesProducerImpl2 {
    type NumericDocValues = DummyNumericDocValues;
    type BinaryDocValues = DummyBinaryDocValues;
    type SortedDocValues = DummySortedDocValues;
    type SortedNumericDocValues = Either2SortedNumericDocValues<
        Either2SortedNumericDocValues<
            SingletonSortedNumericDocValues<BufferedNumericDocValues>,
            BufferedSortedNumericDocValues<DocsWithFieldSetDISI>,
        >,
        SortingSortedNumericDocValues<
            Either2SortedNumericDocValues<
                SingletonSortedNumericDocValues<BufferedNumericDocValues>,
                BufferedSortedNumericDocValues<DocsWithFieldSetDISI>,
            >,
        >,
    >;

    fn get_sorted_numeric(
        &self,
        field_info_in: &Arc<FieldInfo>,
    ) -> Result<Self::SortedNumericDocValues> {
        if !Arc::ptr_eq(&self.field_info, field_info_in) {
            return Err(LuceneError::illegal_state("wrong fieldInfo"));
        }
        let buf = SortedNumericDocValuesWriter::get_values(
            &self.values,
            &self.value_counts,
            &self.docs_with_field,
        )?;
        match &self.sorted {
            Some(sorted) => Ok(Either2SortedNumericDocValues::B(
                SortingSortedNumericDocValues::new(buf, sorted.clone()),
            )),
            None => Ok(Either2SortedNumericDocValues::A(buf)),
        }
    }

    type SortedSetDocValues = DummySortedSetDocValues;
    type DocValuesSkipper = DummyDocValuesSkipper;
}

pub(crate) struct BufferedSortedNumericDocValues<D>
where
    D: DocIdSetIterator,
{
    values_iter: PackedLongValuesIterator,
    value_counts_iter: PackedLongValuesIterator,
    docs_with_field: D,
    value_count: i32,
    value_upto: i32,
}

impl<D> BufferedSortedNumericDocValues<D>
where
    D: DocIdSetIterator,
{
    pub(crate) fn new(
        values: &PackedLongValues,
        value_counts: &PackedLongValues,
        docs_with_field: D,
    ) -> Self {
        Self {
            values_iter: values.iterator(),
            value_counts_iter: value_counts.iterator(),
            docs_with_field,
            value_count: 0,
            value_upto: 0,
        }
    }
}

impl<D> DocValuesIterator for BufferedSortedNumericDocValues<D>
where
    D: DocIdSetIterator,
{
    fn advance_exact(&mut self, _target: i32) -> Result<bool> {
        Err(LuceneError::unsupported_operation(""))
    }
}

impl<D> DocIdSetIterator for BufferedSortedNumericDocValues<D>
where
    D: DocIdSetIterator,
{
    fn doc_id(&self) -> i32 {
        self.docs_with_field.doc_id()
    }

    fn next_doc(&mut self) -> Result<i32> {
        for _ in self.value_upto..self.value_count {
            self.values_iter.next_value();
        }

        let doc_id = self.docs_with_field.next_doc()?;
        if doc_id != NO_MORE_DOCS {
            self.value_count = self.value_counts_iter.next_value().try_into()?;
            self.value_upto = 0;
        }
        Ok(doc_id)
    }

    fn advance(&mut self, _target: i32) -> Result<i32> {
        Err(LuceneError::unsupported_operation(""))
    }

    fn cost(&self) -> Result<i64> {
        self.docs_with_field.cost()
    }
}

impl<D> SortedNumericDocValues for BufferedSortedNumericDocValues<D>
where
    D: DocIdSetIterator,
{
    fn next_value(&mut self) -> Result<i64> {
        self.value_upto += 1;
        Ok(self.values_iter.next_value())
    }

    fn doc_value_count(&mut self) -> Result<i32> {
        Ok(self.value_count)
    }

    type NumericDocValues = DummyNumericDocValues;
}

pub(crate) struct SortingSortedNumericDocValues<S>
where
    S: SortedNumericDocValues,
{
    input: S,
    values: LongValues,
    doc_id: i32,
    upto: i64,
    num_values: i32,
}
impl<S> SortingSortedNumericDocValues<S>
where
    S: SortedNumericDocValues,
{
    pub(crate) fn new(input: S, values: LongValues) -> Self {
        Self {
            input,
            values,
            doc_id: -1,
            upto: 0,
            num_values: -1,
        }
    }
}

impl<S> DocValuesIterator for SortingSortedNumericDocValues<S>
where
    S: SortedNumericDocValues,
{
    fn advance_exact(&mut self, target: i32) -> Result<bool> {
        self.doc_id = target;
        self.upto = self.values.offsets[self.doc_id as usize];

        if self.upto > 0 {
            self.num_values = self.values.values.get(self.upto - 1)?.try_into()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

impl<S> DocIdSetIterator for SortingSortedNumericDocValues<S>
where
    S: SortedNumericDocValues,
{
    fn doc_id(&self) -> i32 {
        self.doc_id
    }

    fn next_doc(&mut self) -> Result<i32> {
        loop {
            self.doc_id += 1;
            if self.doc_id as usize >= self.values.offsets.len() {
                self.doc_id = NO_MORE_DOCS;
                return Ok(self.doc_id);
            }
            let offset = self.values.offsets[self.doc_id as usize];
            if offset > 0 {
                self.upto = offset;
                self.num_values = self.values.values.get(self.upto - 1)?.try_into()?;
                return Ok(self.doc_id);
            }
        }
    }

    fn advance(&mut self, _target: i32) -> Result<i32> {
        Err(LuceneError::unsupported_operation("use nextDoc instead"))
    }

    fn cost(&self) -> Result<i64> {
        self.input.cost()
    }
}

impl<S> SortedNumericDocValues for SortingSortedNumericDocValues<S>
where
    S: SortedNumericDocValues,
{
    fn next_value(&mut self) -> Result<i64> {
        let v = self.values.values.get(self.upto)?;
        self.upto += 1;
        Ok(v)
    }

    fn doc_value_count(&mut self) -> Result<i32> {
        Ok(self.num_values)
    }

    fn is_single_valued(&self) -> bool {
        self.input.is_single_valued()
    }

    type NumericDocValues = S::NumericDocValues;

    fn get_numeric_doc_values(&mut self) -> Result<Option<Self::NumericDocValues>> {
        self.input.get_numeric_doc_values()
    }
}

#[derive(Clone)]
pub(crate) struct LongValues {
    pub(crate) offsets: Rc<Vec<i64>>,
    pub(crate) values: PackedLongValues,
}
impl LongValues {
    pub(crate) fn new<DM>(
        max_doc: usize,
        sort_map: &Rc<DM>,
        old_values: &mut impl SortedNumericDocValues,
        acceptable_overhead_ratio: f32,
    ) -> Result<Self>
    where
        DM: DocMap,
    {
        let mut offsets = vec![0i64; max_doc];
        let mut value_builder =
            PackedLongValues::packed_long_values_builder_default(acceptable_overhead_ratio)?;
        let mut offset_index = 1i64;
        let mut doc_id;
        loop {
            doc_id = old_values.next_doc()?;
            if doc_id == NO_MORE_DOCS {
                break;
            }
            let new_doc_id = sort_map.old_to_new(doc_id);
            let num_values = old_values.doc_value_count()?;
            value_builder.add(num_values as i64)?;
            offsets[new_doc_id as usize] = offset_index;
            offset_index += 1;
            for _ in 0..num_values {
                let value = old_values.next_value()?;
                value_builder.add(value)?;
                offset_index += 1;
            }
        }

        Ok(LongValues {
            offsets: Rc::new(offsets),
            values: value_builder.build()?,
        })
    }
}
