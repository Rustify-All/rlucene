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
use crate::core::codecs::dummy::dummy_sorted_numeric_doc_values::DummySortedNumericDocValues;
use crate::core::codecs::dummy::dummy_sorted_set_doc_values::DummySortedSetDocValues;
use crate::core::index::BytesRef;
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::doc_values_writer::DocValuesWriter;
use crate::core::index::docs_with_field_set::{DocsWithFieldSet, DocsWithFieldSetDISI};
use crate::core::index::field_info::FieldInfo;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::sorted_doc_values::Either2SortedDocValues;
use crate::core::index::sorted_doc_values::SortedDocValues;
use crate::core::index::sorted_doc_values_terms_enum::SortedDocValuesTermsEnum;
use crate::core::index::sorter::DocMap;
use crate::core::search::doc_id_set::DocIdSet;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::store::directory::Directory;
use crate::core::util::accountable::Accountable;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::bytes_ref_hash::{
    BytesRefHash, DEFAULT_CAPACITY, DirectBytesStartArray, MTBytesRefHash,
};
use crate::core::util::error::lucene_error::LuceneError;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::packed::PackedInts;
use crate::core::util::packed::packed_long_values::{
    PackedLongValues, PackedLongValuesBuilder, PackedLongValuesIterator,
};
use crate::core::util::{BYTE_BLOCK_SIZE, ByteBlockPoolLock, CoreHelper, Counter, CounterEnumLock};
use std::borrow::Cow;
use std::fmt::{Display, Formatter};
use std::rc::Rc;
use std::sync::Arc;

///  Buffers up pending `[u8]` per doc, deref and sorting via int ord, then flushes when segment flushes.
pub(crate) struct SortedDocValuesWriter {
    hash: MTBytesRefHash,
    hash_rc: Option<Arc<MTBytesRefHash>>,
    pending: PackedLongValuesBuilder,
    docs_with_field: DocsWithFieldSet,
    iw_bytes_used: CounterEnumLock,
    bytes_used: i64, // this currently only tracks differences in 'pending'
    field_info: Arc<FieldInfo>,
    last_doc_id: i32,

    final_ords: Option<PackedLongValues>,
    // In Java Lucene, `finalSortedValues` corresponds to the `ids` array inside BytesRefHash.
    // Due to language limitations, we do not need to explicitly define finalSortedValues in Rust.
    // Instead of storing the sorted array,
    // we can simply define an `is_sorted` field to indicate whether the BytesRefHash::sort method has been called.
    is_sorted: bool,
    final_ord_map: Option<Arc<Vec<i32>>>,
}

impl SortedDocValuesWriter {
    pub(crate) fn new(
        field_info: Arc<FieldInfo>,
        iw_bytes_used: CounterEnumLock,
        pool: ByteBlockPoolLock,
    ) -> Result<Self> {
        let bytes_start_array =
            DirectBytesStartArray::with_counter_sync(DEFAULT_CAPACITY, iw_bytes_used.clone());
        let hash = BytesRefHash::from_bytes_start_array(pool, DEFAULT_CAPACITY, bytes_start_array);
        let pending =
            PackedLongValues::delta_packed_long_values_builder_default(PackedInts::COMPACT)?;
        let docs_with_field = DocsWithFieldSet::new();
        // TODO: memory calculation not implemented
        let bytes_used = pending.ram_bytes_used()? + docs_with_field.ram_bytes_used()?;
        iw_bytes_used.lock().add_and_get(bytes_used);

        Ok(Self {
            hash,
            hash_rc: None,
            pending,
            docs_with_field,
            iw_bytes_used,
            bytes_used,
            field_info,
            last_doc_id: -1,
            final_ords: None,
            is_sorted: false,
            final_ord_map: None,
        })
    }

    pub(crate) fn add_value(&mut self, doc_id: i32, value: &BytesRef<Vec<u8>>) -> Result<()> {
        if doc_id <= self.last_doc_id {
            return Err(LuceneError::illegal_argument(format!(
                "DocValuesField \"{}\" appears more than once in this document (only one value is allowed per field)",
                self.field_info.name
            )));
        }

        if value.length > (BYTE_BLOCK_SIZE as usize - 2) {
            return Err(LuceneError::illegal_argument(format!(
                "DocValuesField \"{}\" is too large, must be <= {}",
                self.field_info.name,
                BYTE_BLOCK_SIZE - 2
            )));
        }

        self.add_one_value(value)?;
        self.docs_with_field.add(doc_id)?;
        self.last_doc_id = doc_id;
        Ok(())
    }

    fn add_one_value(&mut self, value: &BytesRef<Vec<u8>>) -> Result<()> {
        let mut term_id = self.hash.add(value)?;
        if term_id < 0 {
            term_id = -term_id - 1;
        } else {
            // reserve additional space for each unique value:
            // 1. when indexing, when hash is 50% full, rehash() suddenly needs 2*size ints.
            //    TODO: can this same OOM happen in THPF?
            // 2. when flushing, we need 1 int per value (slot in the ordMap).
            self.iw_bytes_used
                .lock()
                .add_and_get((2 * BitUtil::INT_BYTES) as i64);
        }

        self.pending.add(term_id as i64)?;
        self.update_bytes_used()
    }

    fn update_bytes_used(&mut self) -> Result<()> {
        let new_bytes_used =
            self.pending.ram_bytes_used()? + self.docs_with_field.ram_bytes_used()?;
        let delta = new_bytes_used - self.bytes_used;
        self.iw_bytes_used.lock().add_and_get(delta);
        self.bytes_used = new_bytes_used;
        Ok(())
    }

    fn sort_doc_values<SDV, DM>(
        max_doc: usize,
        sort_map: &DM,
        old_values: &mut SDV,
    ) -> Result<Vec<i32>>
    where
        SDV: SortedDocValues,
        DM: DocMap,
    {
        let mut ords = vec![-1; max_doc];
        let mut doc_id;
        loop {
            doc_id = old_values.next_doc()?;
            if doc_id == NO_MORE_DOCS {
                break;
            }
            let new_doc_id = sort_map.old_to_new(doc_id);
            ords[new_doc_id as usize] = old_values.ord_value()?;
        }
        Ok(ords)
    }
}

impl Display for SortedDocValuesWriter {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", std::any::type_name::<Self>())
    }
}

impl DocValuesWriter for SortedDocValuesWriter {
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
        if !self.is_sorted {
            return Err(LuceneError::illegal_state(
                "must be finished before getting doc values",
            ));
        }
        dv_consumer.add_sorted_field(
            &self.field_info,
            &get_doc_values_producer(
                self.field_info.clone(),
                self.hash_rc.clone().unwrap(),
                self.final_ords.take().unwrap(),
                self.final_ord_map.take().unwrap(),
                std::mem::take(&mut self.docs_with_field),
                sort_map,
            )?,
        )?;
        Ok(())
    }

    type DocIdSetIterator = BufferedSortedDocValues<DocsWithFieldSetDISI>;

    fn get_doc_values(&self) -> Result<Self::DocIdSetIterator> {
        if !self.is_sorted {
            return Err(LuceneError::illegal_state(
                "must be finished before getting doc values",
            ));
        }
        Ok(BufferedSortedDocValues::new(
            self.hash_rc.as_ref().unwrap().clone(),
            self.final_ords.as_ref().unwrap(),
            self.final_ord_map.as_ref().unwrap().clone(),
            self.docs_with_field.iterator()?.unwrap(),
        ))
    }

    fn finish(&mut self) -> Result<()> {
        self.docs_with_field.finish();
        if !self.is_sorted {
            let value_count = self.hash.size();
            self.update_bytes_used()?;
            debug_assert!(self.final_ord_map.is_none() && self.final_ords.is_none());

            self.hash.sort()?;
            self.is_sorted = true;
            let ords = self.pending.build()?;

            let mut ord_map = vec![0i32; value_count as usize];
            for ord in 0..value_count as usize {
                let index = self.hash.ids[ord] as usize;
                ord_map[index] = ord as i32;
            }
            self.hash_rc = Some(Arc::new(std::mem::take(&mut self.hash)));
            self.final_ords = Some(ords);
            self.final_ord_map = Some(Arc::new(ord_map));
        }
        Ok(())
    }
}

pub(crate) struct DocValuesProducerImpl {
    hash: Arc<MTBytesRefHash>,
    ords: PackedLongValues,
    ord_map: Arc<Vec<i32>>,
    docs_with_field: DocsWithFieldSet,
    writer_field_info: Arc<FieldInfo>,
    sorted: Option<Rc<Vec<i32>>>,
}
impl DocValuesProducerImpl {
    pub(crate) fn new(
        hash: Arc<MTBytesRefHash>,
        ords: PackedLongValues,
        ord_map: Arc<Vec<i32>>,
        docs_with_field: DocsWithFieldSet,
        writer_field_info: Arc<FieldInfo>,
        sorted: Option<Rc<Vec<i32>>>,
    ) -> Result<Self> {
        Ok(Self {
            hash,
            ords,
            ord_map,
            docs_with_field,
            writer_field_info,
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
    type BinaryDocValues = DummyBinaryDocValues;
    type SortedDocValues = Either2SortedDocValues<
        BufferedSortedDocValues<DocsWithFieldSetDISI>,
        SortingSortedDocValues<BufferedSortedDocValues<DocsWithFieldSetDISI>>,
    >;

    fn get_sorted(&self, field_info_in: &Arc<FieldInfo>) -> Result<Self::SortedDocValues> {
        if Arc::ptr_eq(&self.writer_field_info, field_info_in) {
            return Err(LuceneError::illegal_argument("wrong fieldInfo"));
        }
        let buf = BufferedSortedDocValues::new(
            self.hash.clone(),
            &self.ords,
            self.ord_map.clone(),
            self.docs_with_field.iterator()?.unwrap(),
        );
        if self.sorted.is_none() {
            return Ok(Either2SortedDocValues::A(buf));
        }
        Ok(Either2SortedDocValues::B(SortingSortedDocValues::new(
            buf,
            self.sorted.as_ref().unwrap().clone(),
        )))
    }

    type SortedNumericDocValues = DummySortedNumericDocValues;
    type SortedSetDocValues = DummySortedSetDocValues;
    type DocValuesSkipper = DummyDocValuesSkipper;
}

pub(crate) struct BufferedSortedDocValues<D>
where
    D: DocIdSetIterator,
{
    hash: Arc<MTBytesRefHash>,
    scratch: BytesRef<Vec<u8>>,
    ord_map: Arc<Vec<i32>>,
    ord: i32,
    iter: PackedLongValuesIterator,
    docs_with_field: D,
}

impl<D> BufferedSortedDocValues<D>
where
    D: DocIdSetIterator,
{
    pub(crate) fn new(
        hash: Arc<MTBytesRefHash>,
        doc_to_ord: &PackedLongValues,
        ord_map: Arc<Vec<i32>>,
        docs_with_field: D,
    ) -> Self {
        Self {
            hash,
            scratch: BytesRef::new(),
            ord_map,
            ord: -1,
            iter: doc_to_ord.iterator(),
            docs_with_field,
        }
    }
}

impl<D> DocValuesIterator for BufferedSortedDocValues<D>
where
    D: DocIdSetIterator,
{
    fn advance_exact(&mut self, _target: i32) -> Result<bool> {
        Err(LuceneError::unsupported_operation(""))
    }
}

impl<D> DocIdSetIterator for BufferedSortedDocValues<D>
where
    D: DocIdSetIterator,
{
    fn doc_id(&self) -> i32 {
        self.docs_with_field.doc_id()
    }

    fn next_doc(&mut self) -> Result<i32> {
        let doc_id = self.docs_with_field.next_doc()?;
        if doc_id != NO_MORE_DOCS {
            let raw_ord: i32 = self.iter.next_value().try_into()?;
            let mapped = self.ord_map[raw_ord as usize];
            self.ord = mapped;
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

impl<D> SortedDocValues for BufferedSortedDocValues<D>
where
    D: DocIdSetIterator,
{
    fn ord_value(&mut self) -> Result<i32> {
        Ok(self.ord)
    }

    fn lookup_ord(&mut self, ord: i32) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
        debug_assert!(ord >= 0 && (ord as usize) < self.hash.ids.len());
        let index = self.hash.ids[ord as usize];
        debug_assert!(
            index >= 0 && (index as usize) < self.hash.ids.len(),
            "sorted_values[ord] out of range"
        );
        self.hash.get(index, &mut self.scratch);
        Ok(Cow::Borrowed(&self.scratch))
    }

    fn get_value_count(&mut self) -> Result<i32> {
        Ok(self.hash.size())
    }

    type TermsEnum = SortedDocValuesTermsEnum;
}

pub(crate) struct SortingSortedDocValues<S>
where
    S: SortedDocValues,
{
    input: S,
    ords: Rc<Vec<i32>>,
    doc_id: i32,
}

impl<S> SortingSortedDocValues<S>
where
    S: SortedDocValues,
{
    pub(crate) fn new(input: S, ords: Rc<Vec<i32>>) -> Self {
        Self {
            input,
            ords,
            doc_id: -1,
        }
    }
}

impl<S> DocValuesIterator for SortingSortedDocValues<S>
where
    S: SortedDocValues,
{
    fn advance_exact(&mut self, target: i32) -> Result<bool> {
        // needed in IndexSorter#StringSorter
        self.doc_id = target;
        Ok(self.ords[target as usize] != -1)
    }
}

impl<S> DocIdSetIterator for SortingSortedDocValues<S>
where
    S: SortedDocValues,
{
    fn doc_id(&self) -> i32 {
        self.doc_id
    }

    fn next_doc(&mut self) -> Result<i32> {
        loop {
            self.doc_id += 1;
            if self.doc_id as usize == self.ords.len() {
                self.doc_id = NO_MORE_DOCS;
                break;
            }
            if self.ords[self.doc_id as usize] != -1 {
                break;
            }
            // skip missing docs
        }
        Ok(self.doc_id)
    }

    fn advance(&mut self, _target: i32) -> Result<i32> {
        Err(LuceneError::unsupported_operation("use next_doc instead"))
    }

    fn cost(&self) -> Result<i64> {
        self.input.cost()
    }
}

impl<S> SortedDocValues for SortingSortedDocValues<S>
where
    S: SortedDocValues,
{
    fn ord_value(&mut self) -> Result<i32> {
        Ok(self.ords[self.doc_id as usize])
    }

    fn lookup_ord(&mut self, ord: i32) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
        self.input.lookup_ord(ord)
    }

    fn get_value_count(&mut self) -> Result<i32> {
        self.input.get_value_count()
    }

    type TermsEnum = SortedDocValuesTermsEnum;
}

pub(crate) fn get_doc_values_producer<DM>(
    writer_field_info: Arc<FieldInfo>,
    hash: Arc<MTBytesRefHash>,
    ords: PackedLongValues,
    ord_map: Arc<Vec<i32>>,
    docs_with_field: DocsWithFieldSet,
    sort_map: Option<Rc<DM>>,
) -> Result<DocValuesProducerImpl>
where
    DM: DocMap,
{
    let sorted = if let Some(sort_map) = sort_map {
        let docs_iter = docs_with_field
            .iterator()?
            .ok_or_else(|| LuceneError::illegal_state("docsWithField.iterator() returned None"))?;
        let mut old_values =
            BufferedSortedDocValues::new(hash.clone(), &ords, ord_map.clone(), docs_iter);
        Some(Rc::new(SortedDocValuesWriter::sort_doc_values(
            sort_map.size() as usize,
            sort_map.as_ref(),
            &mut old_values,
        )?))
    } else {
        None
    };

    DocValuesProducerImpl::new(
        hash,
        ords,
        ord_map,
        docs_with_field,
        writer_field_info,
        sorted,
    )
}
