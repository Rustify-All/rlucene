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
use crate::core::codecs::mutable_point_tree::Either2MutablePointTree;
use crate::core::codecs::mutable_point_tree::MutablePointTree;
use crate::core::codecs::points_reader::PointsReader;
use crate::core::codecs::points_writer::PointsWriter;
use crate::core::index::BytesRef;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::point_values::{IntersectVisitor, PointTree, PointValues, Relation};
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::sorter::DocMap;
use crate::core::store::DataOutput;
use crate::core::store::directory::Directory;
use crate::core::util::accountable::Accountable;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::paged_bytes::{
    PagedBytes, PagedBytesDataOutput, PagedBytesReader, get_data_output,
};
use crate::core::util::{CoreHelper, Counter, CounterEnumLock, SliceCopyOps};
use std::borrow::Cow;
use std::cell::RefCell;
use std::sync::Arc;

/// Buffers up pending byte[][] value(s) per doc, then flushes when segment flushes.
pub(crate) struct PointValuesWriter {
    field_info: Arc<FieldInfo>,
    bytes_out: PagedBytesDataOutput,
    iw_bytes_used: CounterEnumLock,
    doc_ids: Vec<i32>,
    num_points: usize,
    num_docs: usize,
    last_doc_id: i32,
    packed_bytes_length: usize,
}

impl PointValuesWriter {
    pub(crate) fn new(iw_bytes_used: CounterEnumLock, field_info: Arc<FieldInfo>) -> Result<Self> {
        let bytes = PagedBytes::new(12);
        let bytes_out = get_data_output(bytes)?;
        let doc_ids = vec![0; 16];
        iw_bytes_used
            .lock()
            .add_and_get((16 * BitUtil::INT_BYTES) as i64);
        let packed_bytes_length =
            (field_info.get_point_dimension_count() + field_info.get_point_num_bytes()) as usize;
        Ok(Self {
            field_info,
            bytes_out,
            iw_bytes_used,
            doc_ids,
            num_points: 0,
            num_docs: 0,
            last_doc_id: -1,
            packed_bytes_length,
        })
    }
    // TODO: if exactly the same value is added to exactly the same doc, should we dedup?
    pub(crate) fn add_packed_value(
        &mut self,
        doc_id: i32,
        value: &BytesRef<Vec<u8>>,
    ) -> Result<()> {
        if value.length != self.packed_bytes_length {
            return Err(LuceneError::illegal_argument(format!(
                "field={}: this field's value has length={} but should be {}",
                self.field_info.name,
                value.length,
                self.field_info.get_point_dimension_count() + self.field_info.get_point_num_bytes()
            )));
        }

        if self.doc_ids.len() == self.num_points {
            ArrayUtil::grow_with_len(&mut self.doc_ids, self.num_points + 1);
            self.iw_bytes_used
                .lock()
                .add_and_get(((self.doc_ids.len() - self.num_points) * BitUtil::INT_BYTES) as i64);
        }

        let bytes_ram_bytes_used_before = self.bytes_out.paged_bytes.ram_bytes_used()?;
        self.bytes_out
            .write_bytes_range(&value.bytes, value.offset as i32, value.length as i32)?;
        self.iw_bytes_used.lock().add_and_get(
            self.bytes_out.paged_bytes.ram_bytes_used()? - bytes_ram_bytes_used_before,
        );

        self.doc_ids[self.num_points] = doc_id;
        if doc_id != self.last_doc_id {
            self.num_docs += 1;
            self.last_doc_id = doc_id;
        }
        self.num_points += 1;
        Ok(())
    }

    /// Get number of buffered documents.
    pub(crate) fn get_num_docs(&self) -> usize {
        self.num_docs
    }
    pub(crate) fn flush<D, DM, PW>(
        &mut self,
        _state: &SegmentWriteState<D>,
        sort_map: Option<Arc<DM>>,
        writer: &mut PW,
    ) -> Result<()>
    where
        D: Directory,
        DM: DocMap,
        PW: PointsWriter,
    {
        let bytes_reader = self.bytes_out.paged_bytes.freeze(false)?;
        let points = MutablePointTreeImpl::new(
            self.num_points,
            std::mem::take(&mut self.doc_ids),
            bytes_reader,
            self.packed_bytes_length,
        );
        let values = match sort_map {
            Some(doc_map) => {
                Either2MutablePointTree::B(MutableSortingPointValues::new(points, doc_map))
            },
            None => Either2MutablePointTree::A(points),
        };
        let mut reader = PointsReaderImpl::new(values, self.field_info.clone());

        writer.write_field(&self.field_info, &mut reader)
    }
}
struct PointsReaderImpl<DM>
where
    DM: DocMap,
{
    values: RefCell<
        Either2MutablePointTree<
            MutablePointTreeImpl,
            MutableSortingPointValues<MutablePointTreeImpl, DM>,
        >,
    >,
    field_info: Arc<FieldInfo>,
}
impl<DM> PointsReaderImpl<DM>
where
    DM: DocMap,
{
    pub(crate) fn new(
        values: Either2MutablePointTree<
            MutablePointTreeImpl,
            MutableSortingPointValues<MutablePointTreeImpl, DM>,
        >,
        field_info: Arc<FieldInfo>,
    ) -> Self {
        Self {
            values: RefCell::new(values),
            field_info,
        }
    }
}

impl<DM> Clone for PointsReaderImpl<DM>
where
    DM: DocMap,
{
    fn clone(&self) -> Self {
        unreachable!(
            "{} {}",
            std::any::type_name::<Self>(),
            CoreHelper::CLONE_WARRING
        )
    }
}

impl<DM> PointsReader for PointsReaderImpl<DM>
where
    DM: DocMap,
{
    fn check_integrity(&self) -> Result<()> {
        Err(LuceneError::unsupported_operation(""))
    }

    type PointValuesType = PointValuesImpl<DM>;

    fn get_values(&self, field_name: &str) -> Result<Self::PointValuesType> {
        if !field_name.eq(self.field_info.name.as_str()) {
            return Err(LuceneError::illegal_argument("fieldName must be the same"));
        }
        let values = self.values.take();
        Ok(PointValuesImpl::new(values))
    }
}

struct PointValuesImpl<DM>
where
    DM: DocMap,
{
    values: RefCell<
        Either2MutablePointTree<
            MutablePointTreeImpl,
            MutableSortingPointValues<MutablePointTreeImpl, DM>,
        >,
    >,
}
// for padding
impl<DM> Default
    for Either2MutablePointTree<
        MutablePointTreeImpl,
        MutableSortingPointValues<MutablePointTreeImpl, DM>,
    >
where
    DM: DocMap,
{
    fn default() -> Self {
        Either2MutablePointTree::A(MutablePointTreeImpl::default())
    }
}

impl<DM> PointValuesImpl<DM>
where
    DM: DocMap,
{
    pub(crate) fn new(
        values: Either2MutablePointTree<
            MutablePointTreeImpl,
            MutableSortingPointValues<MutablePointTreeImpl, DM>,
        >,
    ) -> Self {
        Self {
            values: RefCell::new(values),
        }
    }
}
impl<DM> PointValues for PointValuesImpl<DM>
where
    DM: DocMap,
{
    fn get_min_packed_value(&self) -> Result<Option<Cow<'_, Vec<u8>>>> {
        Err(LuceneError::unsupported_operation(""))
    }

    fn get_max_packed_value(&self) -> Result<Option<Cow<'_, Vec<u8>>>> {
        Err(LuceneError::unsupported_operation(""))
    }

    fn get_num_dimensions(&self) -> Result<i32> {
        Err(LuceneError::unsupported_operation(""))
    }

    fn get_num_index_dimensions(&self) -> Result<i32> {
        Err(LuceneError::unsupported_operation(""))
    }

    fn get_bytes_per_dimension(&self) -> Result<i32> {
        Err(LuceneError::unsupported_operation(""))
    }

    fn size(&self) -> Result<i64> {
        Err(LuceneError::unsupported_operation(""))
    }

    fn get_doc_count(&self) -> Result<i32> {
        Err(LuceneError::unsupported_operation(""))
    }

    type PointTree = Either2MutablePointTree<
        MutablePointTreeImpl,
        MutableSortingPointValues<MutablePointTreeImpl, DM>,
    >;

    fn get_point_tree(&self) -> Result<Self::PointTree> {
        Ok(self.values.take())
    }
}

pub(crate) struct MutableSortingPointValues<M, DM>
where
    M: MutablePointTree,
    DM: DocMap,
{
    input: M,
    doc_map: Arc<DM>,
}
impl<M, DM> MutableSortingPointValues<M, DM>
where
    M: MutablePointTree,
    DM: DocMap,
{
    pub(crate) fn new(input: M, doc_map: Arc<DM>) -> Self {
        Self { input, doc_map }
    }
}

impl<M, DM> MutablePointTree for MutableSortingPointValues<M, DM>
where
    M: MutablePointTree,
    DM: DocMap,
{
    fn get_value(&self, i: usize, packed_value: &mut BytesRef<Vec<u8>>) {
        self.input.get_value(i, packed_value)
    }

    fn get_byte_at(&self, i: usize, k: usize) -> u8 {
        self.input.get_byte_at(i, k)
    }

    fn get_doc_id(&self, i: usize) -> i32 {
        self.doc_map.old_to_new(self.input.get_doc_id(i))
    }

    fn swap(&mut self, i: usize, j: usize) {
        self.input.swap(i, j);
    }

    fn save(&mut self, i: usize, j: usize) {
        self.input.save(i, j)
    }

    fn restore(&mut self, i: usize, j: usize) {
        self.input.restore(i, j)
    }
}

impl<M, DM> Clone for MutableSortingPointValues<M, DM>
where
    M: MutablePointTree,
    DM: DocMap,
{
    fn clone(&self) -> Self {
        Self {
            input: self.input.clone(),
            doc_map: self.doc_map.clone(),
        }
    }
}

impl<M, DM> PointTree for MutableSortingPointValues<M, DM>
where
    M: MutablePointTree,
    DM: DocMap,
{
    fn size(&self) -> Result<i64> {
        self.input.size()
    }

    fn visit_doc_values<IV>(&mut self, visitor: &mut IV) -> Result<()>
    where
        IV: IntersectVisitor,
    {
        let mut intersect_visitor = IntersectVisitorImpl::new(visitor, self.doc_map.clone());
        self.input.visit_doc_values(&mut intersect_visitor)
    }
}

struct IntersectVisitorImpl<'a, IV, DM>
where
    IV: IntersectVisitor,
    DM: DocMap,
{
    visitor: &'a mut IV,
    doc_map: Arc<DM>,
}
impl<'a, IV, DM> IntersectVisitorImpl<'a, IV, DM>
where
    IV: IntersectVisitor,
    DM: DocMap,
{
    pub(crate) fn new(visitor: &'a mut IV, doc_map: Arc<DM>) -> Self {
        Self { visitor, doc_map }
    }
}
impl<'a, IV, DM> IntersectVisitor for IntersectVisitorImpl<'a, IV, DM>
where
    IV: IntersectVisitor,
    DM: DocMap,
{
    fn visit(&mut self, doc_id: i32) -> Result<()> {
        self.visitor.visit(self.doc_map.old_to_new(doc_id))
    }

    fn visit_with_packed_value(&mut self, doc_id: i32, packed_value: &[u8]) -> Result<()> {
        self.visitor
            .visit_with_packed_value(self.doc_map.old_to_new(doc_id), packed_value)
    }

    fn compare(&mut self, min_packed_value: &[u8], max_packed_value: &[u8]) -> Result<Relation> {
        self.visitor.compare(min_packed_value, max_packed_value)
    }
}

#[derive(Default)]
struct MutablePointTreeImpl {
    num_points: usize,
    ords: Vec<i32>,
    temp: Vec<i32>,
    doc_ids: Vec<i32>,
    packed_bytes_length: usize,
    bytes_reader: PagedBytesReader,
}
impl MutablePointTreeImpl {
    pub(crate) fn new(
        num_points: usize,
        doc_ids: Vec<i32>,
        bytes_reader: PagedBytesReader,
        packed_bytes_length: usize,
    ) -> Self {
        let mut ords: Vec<i32> = vec![0; num_points];
        for (i, ord) in ords.iter_mut().take(num_points).enumerate() {
            *ord = i as i32;
        }
        let temp: Vec<i32> = vec![0; num_points];
        Self {
            num_points,
            ords,
            temp,
            doc_ids,
            packed_bytes_length,
            bytes_reader,
        }
    }
}

impl PointTree for MutablePointTreeImpl {
    fn size(&self) -> Result<i64> {
        Ok(self.num_points as i64)
    }

    fn visit_doc_values<IV>(&mut self, visitor: &mut IV) -> Result<()>
    where
        IV: IntersectVisitor,
    {
        let mut scratch = BytesRef::new();
        let mut packed_value = vec![0u8; self.packed_bytes_length];
        for i in 0..self.num_points {
            self.get_value(i, &mut scratch);
            debug_assert_eq!(scratch.length, self.packed_bytes_length);
            packed_value.copy_from(
                &scratch.bytes[scratch.offset..scratch.offset + self.packed_bytes_length],
                0,
            );
            let doc_id = self.get_doc_id(i);
            visitor.visit_with_packed_value(doc_id, &packed_value)?;
        }
        Ok(())
    }
}

impl Clone for MutablePointTreeImpl {
    fn clone(&self) -> Self {
        let ords = self.ords.clone();
        let temp = self.temp.clone();
        let doc_ids = self.doc_ids.clone();
        let bytes_reader = self.bytes_reader.clone();
        Self {
            num_points: self.num_points,
            ords,
            temp,
            doc_ids,
            packed_bytes_length: self.packed_bytes_length,
            bytes_reader,
        }
    }
}

impl MutablePointTree for MutablePointTreeImpl {
    fn get_value(&self, i: usize, packed_value: &mut BytesRef<Vec<u8>>) {
        let offset = self.packed_bytes_length * self.ords[i] as usize;
        self.bytes_reader
            .fill_slice(packed_value, offset, self.packed_bytes_length);
    }

    fn get_byte_at(&self, i: usize, k: usize) -> u8 {
        let offset = self.packed_bytes_length * self.ords[i] as usize + k;
        self.bytes_reader.get_byte(offset)
    }

    fn get_doc_id(&self, i: usize) -> i32 {
        self.doc_ids[self.ords[i] as usize]
    }

    fn swap(&mut self, i: usize, j: usize) {
        self.ords.swap(i, j);
    }

    fn save(&mut self, i: usize, j: usize) {
        self.temp[j] = self.ords[i];
    }

    fn restore(&mut self, i: usize, j: usize) {
        self.ords.copy_from(&self.temp[i..j], i);
    }
}
