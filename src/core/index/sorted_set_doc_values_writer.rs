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
use crate::core::codecs::dummy::dummy_sorted_numeric_doc_values::DummySortedNumericDocValues;
use crate::core::index::doc_values::DocValues;
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::doc_values_writer::DocValuesWriter;
use crate::core::index::docs_with_field_set::DocsWithFieldSetDISI;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::singleton_sorted_set_doc_values::SingletonSortedSetDocValues;
use crate::core::index::sorted_doc_values::SortedDocValuesEnum2;
use crate::core::index::sorted_doc_values_writer::{
    BufferedSortedDocValues, SortingSortedDocValues, get_doc_values_producer,
};
use crate::core::index::sorted_set_doc_values::SortedSetDocValues;
use crate::core::index::sorted_set_doc_values_terms_enum::SortedSetDocValuesTermsEnum;
use crate::core::index::sorter::DocMap;
use crate::core::index::terms_enum::TermsEnumEnum2;
use crate::core::index::{BytesRef, docs_with_field_set::DocsWithFieldSet, field_info::FieldInfo};
use crate::core::search::doc_id_set::DocIdSet;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::store::directory::Directory;
use crate::core::util::accountable::Accountable;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::bytes_ref_hash::{
    BytesRefHash, DEFAULT_CAPACITY, DirectBytesRefHash, DirectBytesStartArray,
};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::long_values::LongValues;
use crate::core::util::packed::growable_writer::GrowableWriter;
use crate::core::util::packed::packed_long_values::{
    PackedLongValues, PackedLongValuesBuilder, PackedLongValuesIterator,
};
use crate::core::util::packed::{Mutable, PackedInts, Reader};
use crate::core::util::{
    BYTE_BLOCK_SIZE, ByteBlockPool, CoreHelper, Counter, SharedCounter, TryIntoInt,
};
use std::borrow::Cow;
use std::fmt::{Display, Formatter};
use std::rc::Rc;
use std::sync::Arc;

/// Buffers up pending `[u8]`s per doc, deref and sorting via int ord, then flushes when segment flushes.
pub(crate) struct SortedSetDocValuesWriter {
    hash: DirectBytesRefHash,
    hash_rc: Option<Arc<DirectBytesRefHash>>,
    pending: PackedLongValuesBuilder, // stream of all termIDs
    pending_counts: Option<PackedLongValuesBuilder>, // termIDs per doc
    docs_with_field: DocsWithFieldSet,
    iw_bytes_used: SharedCounter,
    bytes_used: i64, // this only tracks differences in 'pending' and 'pendingCounts'
    field_info: Arc<FieldInfo>,

    current_doc: i32,
    current_values: Vec<i32>,
    current_upto: usize,
    max_count: i32,

    final_ords: Option<PackedLongValues>,
    final_ord_counts: Option<PackedLongValues>,
    // In Java Lucene, `finalSortedValues` corresponds to the `ids` array inside BytesRefHash.
    // Due to language limitations, we do not need to explicitly define finalSortedValues in Rust.
    // Instead of storing the sorted array,
    // we can simply define an `is_sorted` field to indicate whether the BytesRefHash::sort method has been called.
    is_sorted: bool,
    final_ord_map: Option<Arc<Vec<i32>>>,
    pool: Arc<ByteBlockPool>,
}

impl SortedSetDocValuesWriter {
    pub(crate) fn new(field_info: Arc<FieldInfo>, iw_bytes_used: SharedCounter) -> Result<Self> {
        let bytes_start_array =
            DirectBytesStartArray::with_counter(DEFAULT_CAPACITY, iw_bytes_used.clone());
        let hash = BytesRefHash::from_bytes_start_array(DEFAULT_CAPACITY, bytes_start_array);
        let pending =
            PackedLongValues::delta_packed_long_values_builder_default(PackedInts::COMPACT)?;
        let docs_with_field = DocsWithFieldSet::new();
        // TODO: memory calculation not implemented
        let bytes_used = pending.ram_bytes_used()? + docs_with_field.ram_bytes_used()?;
        iw_bytes_used.add_and_get(bytes_used);
        Ok(Self {
            hash,
            hash_rc: None,
            pending,
            pending_counts: None,
            docs_with_field,
            iw_bytes_used,
            bytes_used: 0,
            field_info,
            current_doc: -1,
            current_values: Vec::with_capacity(8),
            current_upto: 0,
            max_count: 0,
            final_ords: None,
            final_ord_counts: None,
            is_sorted: false,
            final_ord_map: None,
            pool: Arc::new(ByteBlockPool::default()),
        })
    }

    pub(crate) fn add_value(
        &mut self,
        doc_id: i32,
        value: &BytesRef<Vec<u8>>,
        pool: &mut ByteBlockPool,
    ) -> Result<()> {
        debug_assert!(doc_id >= self.current_doc);
        if value.length > (BYTE_BLOCK_SIZE as usize - 2) {
            return Err(LuceneError::illegal_argument(format!(
                "DocValuesField \"{}\" is too large, must be <= {}",
                self.field_info.name,
                BYTE_BLOCK_SIZE - 2
            )));
        }
        if doc_id != self.current_doc {
            self.finish_current_doc()?;
            self.current_doc = doc_id;
        }
        self.add_one_value(value, pool)?;
        self.update_bytes_used()
    }
    // finalize currentDoc: this deduplicates the current term ids
    fn finish_current_doc(&mut self) -> Result<()> {
        if self.current_doc == -1 {
            return Ok(());
        }
        if self.current_upto > 1 {
            self.current_values[..self.current_upto].sort_unstable();
        }
        let mut last_value = -1;
        let mut count = 0;
        for &term_id in &self.current_values[..self.current_upto] {
            // if it's not a duplicate
            if term_id != last_value {
                self.pending.add(term_id as i64)?;
                count += 1;
            }
            last_value = term_id;
        }
        // record the number of unique term ids for this doc
        if let Some(ref mut pc) = self.pending_counts {
            pc.add(count as i64)?;
        } else if count != 1 {
            let mut pc =
                PackedLongValues::delta_packed_long_values_builder_default(PackedInts::COMPACT)?;
            for _ in 0..self.docs_with_field.cardinality() {
                pc.add(1)?;
            }
            pc.add(count as i64)?;
            self.pending_counts = Some(pc);
        }
        self.max_count = self.max_count.max(count);
        self.current_upto = 0;
        self.docs_with_field.add(self.current_doc)?;
        Ok(())
    }

    fn add_one_value(&mut self, value: &BytesRef<Vec<u8>>, pool: &mut ByteBlockPool) -> Result<()> {
        let mut term_id = self.hash.add(value, pool)?;
        if term_id < 0 {
            term_id = -term_id - 1;
        } else {
            // reserve additional space for each unique value:
            // 1. when indexing, when hash is 50% full, rehash() suddenly needs 2*size ints.
            //    TODO: can this same OOM happen in THPF?
            // 2. when flushing, we need 1 int per value (slot in the ordMap).
            self.iw_bytes_used
                .add_and_get((2 * BitUtil::INT_BYTES) as i64);
        }
        if self.current_upto == self.current_values.len() {
            let old_cap = self.current_values.len();
            ArrayUtil::grow_with_len(&mut self.current_values, old_cap + 1);
            self.iw_bytes_used.add_and_get(
                ((self.current_values.len() - self.current_upto) * BitUtil::INT_BYTES) as i64,
            );
        }
        self.current_values[self.current_upto] = term_id;
        self.current_upto += 1;
        Ok(())
    }

    fn update_bytes_used(&mut self) -> Result<()> {
        let pc_used = if let Some(ref pc) = self.pending_counts {
            pc.ram_bytes_used()?
        } else {
            0
        };
        // TODO: memory calculation not implemented
        let new_used =
            self.pending.ram_bytes_used()? + pc_used + self.docs_with_field.ram_bytes_used()?;
        self.iw_bytes_used.add_and_get(new_used - self.bytes_used);
        self.bytes_used = new_used;
        Ok(())
    }

    pub(crate) fn get_values(
        ord_map: Arc<Vec<i32>>,
        hash: Arc<DirectBytesRefHash>,
        pool: Arc<ByteBlockPool>,
        ords: &PackedLongValues,
        ord_counts: Option<PackedLongValues>,
        max_count: i32,
        docs_with_field: &DocsWithFieldSet,
    ) -> Result<
        SortedSetDocValuesEnum2<
            SingletonSortedSetDocValues<BufferedSortedDocValues<DocsWithFieldSetDISI>>,
            BufferedSortedSetDocValues<DocsWithFieldSetDISI>,
        >,
    > {
        let docs_iter = docs_with_field
            .iterator()?
            .ok_or_else(|| LuceneError::illegal_state("docsWithField.iterator() returned None"))?;
        match ord_counts {
            Some(ords_counts) => Ok(SortedSetDocValuesEnum2::B(BufferedSortedSetDocValues::new(
                ord_map,
                hash,
                pool,
                ords,
                ords_counts,
                max_count,
                docs_iter,
            ))),
            None => Ok(SortedSetDocValuesEnum2::A(DocValues::singleton_sorted(
                BufferedSortedDocValues::new(hash, pool, ords, ord_map, docs_iter),
            )?)),
        }
    }
}

impl Display for SortedSetDocValuesWriter {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", std::any::type_name::<Self>())
    }
}

impl DocValuesWriter for SortedSetDocValuesWriter {
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
        // `final_ords` should always not None here, because we call finish() before flush()
        // but we still keep the check here for consistent with Java Lucene.
        let ords = self.final_ords.take().unwrap();
        let ord_counts = self.final_ord_counts.take();
        let ord_map = self.final_ord_map.take().unwrap();

        if ord_counts.is_none() {
            let single_value_producer = get_doc_values_producer(
                self.field_info.clone(),
                self.hash_rc.clone().unwrap(),
                self.pool.clone(),
                ords.clone(),
                ord_map.clone(),
                std::mem::take(&mut self.docs_with_field),
                sort_map,
            )?;
            let producer = DocValuesProducerImpl2::new(single_value_producer);
            dv_consumer.add_sorted_set_field(&self.field_info, &producer)?;
            return Ok(());
        }

        let doc_ords = if let Some(map) = sort_map {
            Some(DocOrds::new(
                segment_info.max_doc()?,
                map.as_ref(),
                &mut SortedSetDocValuesWriter::get_values(
                    ord_map.clone(),
                    self.hash_rc.clone().unwrap(),
                    self.pool.clone(),
                    &ords,
                    ord_counts.clone(),
                    self.max_count,
                    &self.docs_with_field,
                )?,
                PackedInts::FASTEST,
                PackedInts::bits_required(self.max_count as i64)?,
            )?)
        } else {
            None
        };
        let producer = DocValuesProducerImpl1::new(
            self.field_info.clone(),
            ord_map,
            self.hash_rc.clone().unwrap(),
            self.pool.clone(),
            ords,
            ord_counts,
            self.max_count,
            std::mem::take(&mut self.docs_with_field),
            doc_ords,
        );
        dv_consumer.add_sorted_set_field(&self.field_info, &producer)?;
        Ok(())
    }

    type DocIdSetIterator = SortedSetDocValuesEnum2<
        SingletonSortedSetDocValues<BufferedSortedDocValues<DocsWithFieldSetDISI>>,
        BufferedSortedSetDocValues<DocsWithFieldSetDISI>,
    >;

    fn get_doc_values(&self) -> Result<Self::DocIdSetIterator> {
        if self.final_ords.is_none() {
            return Err(LuceneError::illegal_state(
                "must be finished before getting doc values".to_string(),
            ));
        }
        SortedSetDocValuesWriter::get_values(
            self.final_ord_map.as_ref().unwrap().clone(),
            self.hash_rc.clone().unwrap(),
            self.pool.clone(),
            self.final_ords.as_ref().unwrap(),
            self.final_ord_counts.clone(),
            self.max_count,
            &self.docs_with_field,
        )
    }

    fn finish(&mut self, pool: Arc<ByteBlockPool>) -> Result<()> {
        self.pool = pool;
        if self.final_ords.is_none() {
            debug_assert!(
                self.final_ord_counts.is_none() && !self.is_sorted && self.final_ord_map.is_none()
            );
            self.finish_current_doc()?;
            let value_count = self.hash.size();
            self.final_ords = Some(self.pending.build()?);
            self.final_ord_counts = match std::mem::take(&mut self.pending_counts) {
                Some(mut pc) => Some(pc.build()?),
                None => None,
            };
            self.hash.sort(self.pool.as_ref())?;
            self.is_sorted = true;
            let mut ord_map = vec![0; value_count as usize];
            for ord in 0..value_count as usize {
                let index = self.hash.ids[ord] as usize;
                ord_map[index] = ord as i32;
            }
            self.hash_rc = Some(Arc::new(std::mem::take(&mut self.hash)));
            self.final_ord_map = Some(Arc::new(ord_map));
        } else {
            debug_assert!(self.is_sorted);
        }
        self.docs_with_field.finish();
        Ok(())
    }
}
pub(crate) struct DocValuesProducerImpl1 {
    field_info: Arc<FieldInfo>,
    ord_map: Arc<Vec<i32>>,
    hash: Arc<DirectBytesRefHash>,
    pool: Arc<ByteBlockPool>,
    ords: PackedLongValues,
    ord_counts: Option<PackedLongValues>,
    max_count: i32,
    docs_with_field: DocsWithFieldSet,
    doc_ords: Option<DocOrds>,
}
impl DocValuesProducerImpl1 {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        field_info: Arc<FieldInfo>,
        ord_map: Arc<Vec<i32>>,
        hash: Arc<DirectBytesRefHash>,
        pool: Arc<ByteBlockPool>,
        ords: PackedLongValues,
        ord_counts: Option<PackedLongValues>,
        max_count: i32,
        docs_with_field: DocsWithFieldSet,
        doc_ords: Option<DocOrds>,
    ) -> Self {
        Self {
            field_info,
            ord_map,
            hash,
            pool,
            ords,
            ord_counts,
            max_count,
            docs_with_field,
            doc_ords,
        }
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
    type SortedNumericDocValues = DummySortedNumericDocValues;
    type SortedSetDocValues = SortedSetDocValuesEnum2<
        SortedSetDocValuesEnum2<
            SingletonSortedSetDocValues<BufferedSortedDocValues<DocsWithFieldSetDISI>>,
            BufferedSortedSetDocValues<DocsWithFieldSetDISI>,
        >,
        SortingSortedSetDocValues<
            SortedSetDocValuesEnum2<
                SingletonSortedSetDocValues<BufferedSortedDocValues<DocsWithFieldSetDISI>>,
                BufferedSortedSetDocValues<DocsWithFieldSetDISI>,
            >,
        >,
    >;

    fn get_sorted_set(&self, field_info: &Arc<FieldInfo>) -> Result<Self::SortedSetDocValues> {
        if !Arc::ptr_eq(&self.field_info, field_info) {
            return Err(LuceneError::illegal_argument("wrong fieldInfo"));
        }
        let buf = SortedSetDocValuesWriter::get_values(
            self.ord_map.clone(),
            self.hash.clone(),
            self.pool.clone(),
            &self.ords,
            self.ord_counts.clone(),
            self.max_count,
            &self.docs_with_field,
        )?;
        match &self.doc_ords {
            Some(ords) => Ok(SortedSetDocValuesEnum2::B(SortingSortedSetDocValues::new(
                buf,
                ords.clone(),
            ))),
            None => Ok(SortedSetDocValuesEnum2::A(buf)),
        }
    }

    type DocValuesSkipper = DummyDocValuesSkipper;
}

pub(crate) struct DocValuesProducerImpl2 {
    single_value_producer: crate::core::index::sorted_doc_values_writer::DocValuesProducerImpl,
}
impl DocValuesProducerImpl2 {
    pub(crate) fn new(
        single_value_producer: crate::core::index::sorted_doc_values_writer::DocValuesProducerImpl,
    ) -> Self {
        Self {
            single_value_producer,
        }
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
    type SortedNumericDocValues = DummySortedNumericDocValues;
    type SortedSetDocValues = SingletonSortedSetDocValues<
        SortedDocValuesEnum2<
            BufferedSortedDocValues<DocsWithFieldSetDISI>,
            SortingSortedDocValues<BufferedSortedDocValues<DocsWithFieldSetDISI>>,
        >,
    >;

    fn get_sorted_set(&self, field_info: &Arc<FieldInfo>) -> Result<Self::SortedSetDocValues> {
        DocValues::singleton_sorted(self.single_value_producer.get_sorted(field_info)?)
    }

    type DocValuesSkipper = DummyDocValuesSkipper;
}

pub(crate) struct BufferedSortedSetDocValues<D>
where
    D: DocIdSetIterator,
{
    ord_map: Arc<Vec<i32>>,
    hash: Arc<DirectBytesRefHash>,
    pool: Arc<ByteBlockPool>,
    scratch: BytesRef<Vec<u8>>,
    ords_iter: PackedLongValuesIterator,
    ord_counts_iter: PackedLongValuesIterator,
    docs_with_field: D,
    current_doc: Vec<i32>,
    ord_count: usize,
    ord_upto: usize,
}

impl<D> BufferedSortedSetDocValues<D>
where
    D: DocIdSetIterator,
{
    pub(crate) fn new(
        ord_map: Arc<Vec<i32>>,
        hash: Arc<DirectBytesRefHash>,
        pool: Arc<ByteBlockPool>,
        ords: &PackedLongValues,
        ord_counts: PackedLongValues,
        max_count: i32,
        docs_with_field: D,
    ) -> Self {
        Self {
            ord_map,
            hash,
            pool,
            scratch: BytesRef::new(),
            ords_iter: ords.iterator(),
            ord_counts_iter: ord_counts.iterator(),
            docs_with_field,
            current_doc: vec![0; max_count as usize],
            ord_count: 0,
            ord_upto: 0,
        }
    }
}

impl<D> DocIdSetIterator for BufferedSortedSetDocValues<D>
where
    D: DocIdSetIterator,
{
    fn doc_id(&self) -> i32 {
        self.docs_with_field.doc_id()
    }

    fn next_doc(&mut self) -> Result<i32> {
        let doc_id = self.docs_with_field.next_doc()?;
        if doc_id != NO_MORE_DOCS {
            let count = self.ord_counts_iter.next_value() as usize;
            debug_assert!(count > 0);
            self.ord_count = count;
            for i in 0..count {
                let raw: i32 = self.ords_iter.next_value().try_convert()?;
                self.current_doc[i] = self.ord_map[raw as usize];
            }
            self.current_doc[..count].sort_unstable();
            self.ord_upto = 0;
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

impl<D> DocValuesIterator for BufferedSortedSetDocValues<D>
where
    D: DocIdSetIterator,
{
    fn advance_exact(&mut self, _target: i32) -> Result<bool> {
        Err(LuceneError::unsupported_operation(""))
    }
}

impl<D> SortedSetDocValues for BufferedSortedSetDocValues<D>
where
    D: DocIdSetIterator,
{
    fn next_ord(&mut self) -> Result<i64> {
        let ord = self.current_doc[self.ord_upto] as i64;
        self.ord_upto += 1;
        Ok(ord)
    }

    fn doc_value_count(&mut self) -> Result<i32> {
        Ok(self.ord_count as i32)
    }

    fn lookup_ord(&mut self, ord: i64) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
        debug_assert!(ord >= 0 && (ord as usize) < self.ord_map.len());
        let idx: i32 = ord.try_convert()?;
        let hash_idx = self.hash.ids[idx as usize];
        self.hash
            .get(hash_idx, &mut self.scratch, self.pool.as_ref());
        Ok(Cow::Borrowed(&self.scratch))
    }

    fn get_value_count(&self) -> Result<i64> {
        Ok(self.ord_map.len() as i64)
    }

    type TermsEnumRef<'a>
        = SortedSetDocValuesTermsEnum<&'a mut Self>
    where
        D: 'a;

    type TermsEnum = SortedSetDocValuesTermsEnum<Self>;

    fn terms_enum(&mut self) -> Result<Self::TermsEnumRef<'_>> {
        self.default_terms_enum()
    }

    fn take_terms_enum(self) -> Result<Self::TermsEnum> {
        self.default_take_terms_enum()
    }

    type SortedDocValues = DummySortedDocValues;
}

pub(crate) struct SortingSortedSetDocValues<S>
where
    S: SortedSetDocValues,
{
    input: S,
    ords: DocOrds,
    doc_id: i32,
    ord_upto: usize,
    count: i32,
}

impl<S> SortingSortedSetDocValues<S>
where
    S: SortedSetDocValues,
{
    pub(crate) fn new(input: S, ords: DocOrds) -> Self {
        Self {
            input,
            ords,
            doc_id: -1,
            ord_upto: 0,
            count: 0,
        }
    }

    fn init_count(&mut self) -> Result<()> {
        debug_assert!(self.ord_upto > 0);
        self.ord_upto = self.ords.offsets[self.doc_id as usize] - 1;
        self.count = self
            .ords
            .doc_value_counts
            .get(self.doc_id.try_convert()?)
            .try_convert()?;
        Ok(())
    }
}

impl<S> DocValuesIterator for SortingSortedSetDocValues<S>
where
    S: SortedSetDocValues,
{
    fn advance_exact(&mut self, target: i32) -> Result<bool> {
        // needed in IndexSorter#StringSorter
        self.doc_id = target;
        self.init_count()?;
        Ok(self.ords.offsets[self.doc_id as usize] > 0)
    }
}

impl<S> DocIdSetIterator for SortingSortedSetDocValues<S>
where
    S: SortedSetDocValues,
{
    fn doc_id(&self) -> i32 {
        self.doc_id
    }

    fn next_doc(&mut self) -> Result<i32> {
        loop {
            self.doc_id += 1;
            if (self.doc_id as usize) == self.ords.offsets.len() {
                self.doc_id = NO_MORE_DOCS;
                break;
            }
            if self.ords.offsets[self.doc_id as usize] > 0 {
                break;
            }
        }
        self.init_count()?;
        Ok(self.doc_id)
    }

    fn advance(&mut self, _target: i32) -> Result<i32> {
        Err(LuceneError::unsupported_operation("use next_doc instead"))
    }

    fn cost(&self) -> Result<i64> {
        self.input.cost()
    }
}

impl<S> SortedSetDocValues for SortingSortedSetDocValues<S>
where
    S: SortedSetDocValues,
{
    fn next_ord(&mut self) -> Result<i64> {
        let ord = self.ords.ords.get(self.ord_upto)?;
        self.ord_upto += 1;
        Ok(ord)
    }

    fn doc_value_count(&mut self) -> Result<i32> {
        debug_assert!(self.doc_id >= 0);
        Ok(self.count)
    }

    fn lookup_ord(&mut self, ord: i64) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
        self.input.lookup_ord(ord)
    }

    fn get_value_count(&self) -> Result<i64> {
        self.input.get_value_count()
    }

    type TermsEnumRef<'a>
        = SortedSetDocValuesTermsEnum<&'a mut Self>
    where
        S: 'a;

    type TermsEnum = SortedSetDocValuesTermsEnum<Self>;

    fn terms_enum(&mut self) -> Result<Self::TermsEnumRef<'_>> {
        self.default_terms_enum()
    }

    fn take_terms_enum(self) -> Result<Self::TermsEnum> {
        self.default_take_terms_enum()
    }

    fn is_single_valued(&self) -> bool {
        self.input.is_single_valued()
    }

    type SortedDocValues = S::SortedDocValues;

    fn get_sorted_doc_values(&mut self) -> Result<Self::SortedDocValues> {
        self.input.get_sorted_doc_values()
    }
}

#[derive(Clone)]
pub(crate) struct DocOrds {
    pub(crate) offsets: Rc<Vec<usize>>,
    pub(crate) ords: PackedLongValues,
    pub(crate) doc_value_counts: Rc<GrowableWriter>,
}

impl DocOrds {
    pub(crate) fn new<DM>(
        max_doc: i32,
        sort_map: &DM,
        old_values: &mut impl SortedSetDocValues,
        acceptable_overhead_ratio: f32,
        bits_per_value: i32,
    ) -> Result<Self>
    where
        DM: DocMap,
    {
        let mut offsets = vec![0; max_doc as usize];
        let mut builder =
            PackedLongValues::packed_long_values_builder_default(acceptable_overhead_ratio)?;
        let mut doc_value_counts =
            GrowableWriter::new(bits_per_value, max_doc, acceptable_overhead_ratio);
        let mut ord_offset = 1;
        loop {
            let doc_id = old_values.next_doc()?;
            if doc_id == NO_MORE_DOCS {
                break;
            }

            let new_doc_id = sort_map.old_to_new(doc_id)?;
            let start_offset = ord_offset;
            let doc_value_count = old_values.doc_value_count()?;
            ord_offset += doc_value_count as usize;

            for _ in 0..doc_value_count {
                builder.add(old_values.next_ord()?)?;
            }

            doc_value_counts.set(new_doc_id, (ord_offset - start_offset) as i64);

            if start_offset != ord_offset {
                // do we have any values?
                offsets[new_doc_id as usize] = start_offset;
            }
        }
        let ords = builder.build()?;

        Ok(DocOrds {
            offsets: Rc::new(offsets),
            ords,
            doc_value_counts: Rc::new(doc_value_counts),
        })
    }
}

// SortedSetDocValues
pub enum SortedSetDocValuesEnum2<A, B> {
    A(A),
    B(B),
}

impl<A, B> DocValuesIterator for SortedSetDocValuesEnum2<A, B>
where
    A: SortedSetDocValues,
    B: SortedSetDocValues,
{
    fn advance_exact(&mut self, target: i32) -> Result<bool> {
        match self {
            SortedSetDocValuesEnum2::A(t) => t.advance_exact(target),
            SortedSetDocValuesEnum2::B(s) => s.advance_exact(target),
        }
    }
}

impl<A, B> DocIdSetIterator for SortedSetDocValuesEnum2<A, B>
where
    A: SortedSetDocValues,
    B: SortedSetDocValues,
{
    fn doc_id(&self) -> i32 {
        match self {
            SortedSetDocValuesEnum2::A(t) => t.doc_id(),
            SortedSetDocValuesEnum2::B(s) => s.doc_id(),
        }
    }

    fn next_doc(&mut self) -> Result<i32> {
        match self {
            SortedSetDocValuesEnum2::A(t) => t.next_doc(),
            SortedSetDocValuesEnum2::B(s) => s.next_doc(),
        }
    }

    fn advance(&mut self, target: i32) -> Result<i32> {
        match self {
            SortedSetDocValuesEnum2::A(t) => t.advance(target),
            SortedSetDocValuesEnum2::B(s) => s.advance(target),
        }
    }

    fn slow_advance(&mut self, target: i32) -> Result<i32> {
        match self {
            SortedSetDocValuesEnum2::A(t) => t.slow_advance(target),
            SortedSetDocValuesEnum2::B(s) => s.slow_advance(target),
        }
    }

    fn cost(&self) -> Result<i64> {
        match self {
            SortedSetDocValuesEnum2::A(t) => t.cost(),
            SortedSetDocValuesEnum2::B(s) => s.cost(),
        }
    }
}

impl<A, B> SortedSetDocValues for SortedSetDocValuesEnum2<A, B>
where
    A: SortedSetDocValues,
    B: SortedSetDocValues,
{
    fn next_ord(&mut self) -> Result<i64> {
        match self {
            SortedSetDocValuesEnum2::A(t) => t.next_ord(),
            SortedSetDocValuesEnum2::B(s) => s.next_ord(),
        }
    }

    fn doc_value_count(&mut self) -> Result<i32> {
        match self {
            SortedSetDocValuesEnum2::A(t) => t.doc_value_count(),
            SortedSetDocValuesEnum2::B(s) => s.doc_value_count(),
        }
    }

    fn lookup_ord(&mut self, _ord: i64) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
        match self {
            SortedSetDocValuesEnum2::A(t) => t.lookup_ord(_ord),
            SortedSetDocValuesEnum2::B(s) => s.lookup_ord(_ord),
        }
    }

    fn get_value_count(&self) -> Result<i64> {
        match self {
            SortedSetDocValuesEnum2::A(t) => t.get_value_count(),
            SortedSetDocValuesEnum2::B(s) => s.get_value_count(),
        }
    }

    fn lookup_term(&mut self, key: &BytesRef<Vec<u8>>) -> Result<i64> {
        match self {
            SortedSetDocValuesEnum2::A(t) => t.lookup_term(key),
            SortedSetDocValuesEnum2::B(s) => s.lookup_term(key),
        }
    }

    type TermsEnumRef<'a>
        = TermsEnumEnum2<A::TermsEnumRef<'a>, B::TermsEnumRef<'a>>
    where
        A: 'a,
        B: 'a;

    type TermsEnum = TermsEnumEnum2<A::TermsEnum, B::TermsEnum>;

    fn terms_enum(&mut self) -> Result<Self::TermsEnumRef<'_>> {
        match self {
            SortedSetDocValuesEnum2::A(t) => {
                let terms_enum = t.terms_enum()?;
                Ok(TermsEnumEnum2::A(terms_enum))
            },
            SortedSetDocValuesEnum2::B(s) => {
                let terms_enum = s.terms_enum()?;
                Ok(TermsEnumEnum2::B(terms_enum))
            },
        }
    }

    fn take_terms_enum(self) -> Result<Self::TermsEnum> {
        match self {
            SortedSetDocValuesEnum2::A(t) => {
                let terms_enum = t.take_terms_enum()?;
                Ok(TermsEnumEnum2::A(terms_enum))
            },
            SortedSetDocValuesEnum2::B(s) => {
                let terms_enum = s.take_terms_enum()?;
                Ok(TermsEnumEnum2::B(terms_enum))
            },
        }
    }

    fn is_single_valued(&self) -> bool {
        match self {
            SortedSetDocValuesEnum2::A(t) => t.is_single_valued(),
            SortedSetDocValuesEnum2::B(s) => s.is_single_valued(),
        }
    }

    type SortedDocValues = SortedDocValuesEnum2<A::SortedDocValues, B::SortedDocValues>;

    fn get_sorted_doc_values(&mut self) -> Result<Self::SortedDocValues> {
        match self {
            SortedSetDocValuesEnum2::A(t) => {
                Ok(SortedDocValuesEnum2::A(t.get_sorted_doc_values()?))
            },
            SortedSetDocValuesEnum2::B(s) => {
                Ok(SortedDocValuesEnum2::B(s.get_sorted_doc_values()?))
            },
        }
    }
}
