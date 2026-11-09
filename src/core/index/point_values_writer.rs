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
use crate::core::codecs::mutable_point_tree::MutablePointTree;
use crate::core::codecs::points_reader::PointsReader;
use crate::core::codecs::points_writer::PointsWriter;
use crate::core::index::BytesRef;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::point_values::{
  IntersectVisitor, PointTree, PointTreeEnum, PointValues, Relation,
};
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::sorter::DocMap;
use crate::core::store::DataOutput;
use crate::core::store::directory::Directory;
use crate::core::util::accountable::Accountable;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::clone::TryClone;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::paged_bytes::{PagedBytes, PagedBytesDataOutput, Reader, get_data_output};
use crate::core::util::{Counter, SharedCounter, SliceCopyOps, TryIntoInt};
use std::borrow::Cow;
use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::Relaxed;

/// Buffers pending byte values per document, then writes them during a segment flush.
pub(crate) struct PointValuesWriter {
  field_info: Arc<FieldInfo>,
  bytes_out: PagedBytesDataOutput,
  iw_bytes_used: SharedCounter,
  doc_ids: Vec<i32>,
  num_docs: usize,
  packed_bytes_length: usize,
}

impl PointValuesWriter {
  pub(crate) fn new(iw_bytes_used: SharedCounter, field_info: Arc<FieldInfo>) -> Result<Self> {
    let bytes = PagedBytes::new(12);
    let bytes_out = get_data_output(bytes)?;
    let doc_ids = Vec::with_capacity(16);
    iw_bytes_used.add_and_get((16 * BitUtil::INT_BYTES) as i64);
    let packed_bytes_length =
      field_info.get_point_dimension_count() * field_info.get_point_num_bytes();
    Ok(Self {
      field_info,
      bytes_out,
      iw_bytes_used,
      doc_ids,
      num_docs: 0,
      packed_bytes_length,
    })
  }
  pub(crate) fn add_packed_value(&mut self, doc_id: i32, value: &BytesRef<&[u8]>) -> Result<()> {
    if value.length != self.packed_bytes_length {
      return Err(LuceneError::illegal_argument(format!(
        "field={}: this field's value has length={} but should be {}",
        self.field_info.name,
        value.length,
        self.field_info.get_point_dimension_count() * self.field_info.get_point_num_bytes()
      )));
    }

    if self.doc_ids.len() == self.doc_ids.capacity() {
      let old_capacity = self.doc_ids.capacity();
      let min_size = self.doc_ids.len() + 1;
      ArrayUtil::grow_capacity(&mut self.doc_ids, min_size)?;
      self
        .iw_bytes_used
        .add_and_get(((self.doc_ids.capacity() - old_capacity) * BitUtil::INT_BYTES) as i64);
    }

    let bytes_ram_bytes_used_before = self.bytes_out.paged_bytes.ram_bytes_used()?;
    self
      .bytes_out
      .write_bytes_range(value.bytes, value.offset, value.length)?;
    self
      .iw_bytes_used
      .add_and_get(self.bytes_out.paged_bytes.ram_bytes_used()? - bytes_ram_bytes_used_before);

    if doc_id != self.doc_ids.last().copied().unwrap_or(-1) {
      self.num_docs += 1;
    }
    self.doc_ids.push(doc_id);
    Ok(())
  }

  /// Get number of buffered documents.
  #[allow(dead_code)]
  pub(crate) fn get_num_docs(&self) -> usize {
    self.num_docs
  }
  pub(crate) fn flush<D1, D2, DM, PW>(
    &mut self,
    state: &D1,
    sort_map: Option<&DM>,
    writer: &mut PW,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<()>
  where
    D1: Directory,
    DM: DocMap,
    PW: PointsWriter,
  {
    let bytes_reader = self.bytes_out.paged_bytes.freeze(false)?;
    let doc_ids = std::mem::take(&mut self.doc_ids);
    let points = MutablePointTreeImpl::new(
      doc_ids.len(),
      doc_ids,
      bytes_reader,
      self.packed_bytes_length,
    )?;
    match sort_map {
      Some(doc_map) => {
        let values = MutableSortingPointValues::new(points, doc_map);
        let mut reader = PointsReaderImpl::new(
          values,
          move || MutableSortingPointValues::new(MutablePointTreeImpl::default(), doc_map),
          self.field_info.as_ref(),
        );
        writer.write_field(&self.field_info, &mut reader, state, segment_info)
      },
      None => {
        let mut reader = PointsReaderImpl::new(
          points,
          MutablePointTreeImpl::default,
          self.field_info.as_ref(),
        );
        writer.write_field(&self.field_info, &mut reader, state, segment_info)
      },
    }
  }
}
struct PointsReaderImpl<'a, M, F> {
  values: RefCell<M>,
  make_empty: F,
  field_info: &'a FieldInfo,
}

impl<M, F> CloseableRef for PointsReaderImpl<'_, M, F> {}

impl<'a, M, F> PointsReaderImpl<'a, M, F> {
  pub(crate) fn new(values: M, make_empty: F, field_info: &'a FieldInfo) -> Self {
    Self {
      values: RefCell::new(values),
      make_empty,
      field_info,
    }
  }
}

impl<M, F> PointsReader for PointsReaderImpl<'_, M, F>
where
  M: MutablePointTree,
  F: Fn() -> M + Copy,
{
  fn check_integrity(&self) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }

  type PointValuesType = PointValuesImpl<M, F>;

  fn get_values(&self, field_name: &str) -> Result<Option<Self::PointValuesType>> {
    if !field_name.eq(self.field_info.name.as_str()) {
      return Err(LuceneError::illegal_argument("fieldName must be the same"));
    }
    let values = self.values.replace((self.make_empty)());
    Ok(Some(PointValuesImpl::new(values, self.make_empty)))
  }
}

struct PointValuesImpl<M, F> {
  values: RefCell<M>,
  make_empty: F,
  taken: AtomicBool,
}

impl<M, F> PointValuesImpl<M, F> {
  pub(crate) fn new(values: M, make_empty: F) -> Self {
    Self {
      values: RefCell::new(values),
      make_empty,
      taken: AtomicBool::new(false),
    }
  }
}
impl<M, F> PointValues for PointValuesImpl<M, F>
where
  M: MutablePointTree,
  F: Fn() -> M + Copy,
{
  fn get_min_packed_value(&self) -> Result<Option<Cow<'_, [u8]>>> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn get_max_packed_value(&self) -> Result<Option<Cow<'_, [u8]>>> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn get_num_dimensions(&self) -> Result<usize> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn get_num_index_dimensions(&self) -> Result<usize> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn get_bytes_per_dimension(&self) -> Result<usize> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn size(&self) -> Result<usize> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn get_doc_count(&self) -> Result<i32> {
    Err(LuceneError::unsupported_operation(""))
  }

  type PointTree = M;

  fn get_point_tree(&self) -> Result<Self::PointTree> {
    if self.taken.swap(true, Relaxed) {
      return Err(LuceneError::illegal_state(format!(
        "{} get_point_tree can only be called once",
        std::any::type_name::<Self>()
      )));
    }
    Ok(self.values.replace((self.make_empty)()))
  }
}

pub(crate) struct MutableSortingPointValues<M, DM> {
  input: M,
  doc_map: DM,
}
impl<M, DM> MutableSortingPointValues<M, DM> {
  pub(crate) fn new(input: M, doc_map: DM) -> Self {
    Self { input, doc_map }
  }
}

impl<M, DM> MutablePointTree for MutableSortingPointValues<M, DM>
where
  M: MutablePointTree,
  DM: DocMap + Clone,
{
  fn get_value<'a>(
    &'a self,
    i: usize,
    packed_value: &'a mut BytesRef<Vec<u8>>,
  ) -> Result<BytesRef<&'a [u8]>> {
    self.input.get_value(i, packed_value)
  }

  fn get_byte_at(&self, i: usize, k: usize) -> u8 {
    self.input.get_byte_at(i, k)
  }

  fn get_doc_id(&self, i: usize) -> Result<i32> {
    let doc_id = self.input.get_doc_id(i)?;
    self.doc_map.old_to_new(doc_id)
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

impl<M, DM> TryClone for MutableSortingPointValues<M, DM>
where
  M: MutablePointTree,
  DM: DocMap + Clone,
{
  fn try_clone(&self) -> Result<Self>
  where
    Self: Sized,
  {
    Ok(Self {
      input: self.input.try_clone()?,
      doc_map: self.doc_map.clone(),
    })
  }
}

impl<M, DM> PointTree for MutableSortingPointValues<M, DM>
where
  M: MutablePointTree,
  DM: DocMap + Clone,
{
  type MutablePointTree = Self;

  fn into_mutable(self) -> PointTreeEnum<Self::MutablePointTree, Self> {
    PointTreeEnum::Mutable(self)
  }

  fn size(&self) -> Result<usize> {
    self.input.size()
  }

  fn visit_doc_values<IV>(&mut self, visitor: &mut IV) -> Result<()>
  where
    IV: IntersectVisitor,
  {
    let mut intersect_visitor = IntersectVisitorImpl::new(visitor, &self.doc_map);
    self.input.visit_doc_values(&mut intersect_visitor)
  }
}

struct IntersectVisitorImpl<'a, IV, DM> {
  visitor: &'a mut IV,
  doc_map: &'a DM,
}
impl<'a, IV, DM> IntersectVisitorImpl<'a, IV, DM> {
  pub(crate) fn new(visitor: &'a mut IV, doc_map: &'a DM) -> Self {
    Self { visitor, doc_map }
  }
}
impl<'a, IV, DM> IntersectVisitor for IntersectVisitorImpl<'a, IV, DM>
where
  IV: IntersectVisitor,
  DM: DocMap,
{
  fn visit(&mut self, doc_id: i32) -> Result<()> {
    self.visitor.visit(self.doc_map.old_to_new(doc_id)?)
  }

  fn visit_with_packed_value(&mut self, doc_id: i32, packed_value: &[u8]) -> Result<()> {
    self
      .visitor
      .visit_with_packed_value(self.doc_map.old_to_new(doc_id)?, packed_value)
  }

  fn compare(&self, min_packed_value: &[u8], max_packed_value: &[u8]) -> Result<Relation> {
    self.visitor.compare(min_packed_value, max_packed_value)
  }
}

#[derive(Default)]
struct MutablePointTreeImpl {
  num_points: usize,
  ords: Vec<u32>,
  temp: Option<Vec<u32>>,
  doc_ids: Vec<i32>,
  packed_bytes_length: usize,
  bytes_reader: Reader,
}
impl MutablePointTreeImpl {
  pub(crate) fn new(
    num_points: usize,
    doc_ids: Vec<i32>,
    bytes_reader: Reader,
    packed_bytes_length: usize,
  ) -> Result<Self> {
    let end: u32 = num_points.try_convert()?;
    let ords = (0..end).collect();
    Ok(Self {
      num_points,
      ords,
      temp: None,
      doc_ids,
      packed_bytes_length,
      bytes_reader,
    })
  }
}

impl PointTree for MutablePointTreeImpl {
  type MutablePointTree = Self;

  fn into_mutable(self) -> PointTreeEnum<Self::MutablePointTree, Self> {
    PointTreeEnum::Mutable(self)
  }

  fn size(&self) -> Result<usize> {
    Ok(self.num_points)
  }

  fn visit_doc_values<IV>(&mut self, visitor: &mut IV) -> Result<()>
  where
    IV: IntersectVisitor,
  {
    let mut scratch = BytesRef::new();
    for i in 0..self.num_points {
      let packed_value = self.get_value(i, &mut scratch)?;
      debug_assert_eq!(packed_value.length, self.packed_bytes_length);
      let packed_value =
        &packed_value.bytes[packed_value.offset..packed_value.offset + self.packed_bytes_length];
      let doc_id = self.get_doc_id(i)?;
      visitor.visit_with_packed_value(doc_id, packed_value)?;
    }
    Ok(())
  }
}
impl TryClone for MutablePointTreeImpl {
  fn try_clone(&self) -> Result<Self>
  where
    Self: Sized,
  {
    Ok(self.clone())
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
  fn get_value<'a>(
    &'a self,
    i: usize,
    packed_value: &'a mut BytesRef<Vec<u8>>,
  ) -> Result<BytesRef<&'a [u8]>> {
    let offset = self.packed_bytes_length * self.ords[i] as usize;
    self
      .bytes_reader
      .fill_slice(packed_value, offset, self.packed_bytes_length)
  }

  fn get_byte_at(&self, i: usize, k: usize) -> u8 {
    let offset = self.packed_bytes_length * self.ords[i] as usize + k;
    self.bytes_reader.get_byte(offset)
  }

  fn get_doc_id(&self, i: usize) -> Result<i32> {
    Ok(self.doc_ids[self.ords[i] as usize])
  }

  fn swap(&mut self, i: usize, j: usize) {
    self.ords.swap(i, j);
  }

  fn save(&mut self, i: usize, j: usize) {
    let temp = self.temp.get_or_insert_with(|| vec![0; self.ords.len()]);
    temp[j] = self.ords[i];
  }

  fn restore(&mut self, i: usize, j: usize) {
    if let Some(temp) = &self.temp {
      self.ords.copy_from(&temp[i..j], i);
    }
  }
}
