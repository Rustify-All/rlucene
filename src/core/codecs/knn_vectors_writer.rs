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
use crate::core::codecs::knn_field_vectors_writer::VectorInput;
use crate::core::codecs::knn_vectors_reader::KnnVectorsReader;
use crate::core::index::byte_vector_values::ByteVectorValue;
use crate::core::index::byte_vector_values::ByteVectorValues;
use crate::core::index::codec_reader::CRKnnVectorReader;
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::docs_with_field_set::DocsWithFieldSet;
use crate::core::index::dummy::dummy_byte_vector_values::DummyByteVectorValues;
use crate::core::index::dummy::dummy_float_vector_values::DummyFloatVectorValues;
use crate::core::index::dummy::dummy_knn_vector_values::DummyKnnVectorsWriter;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::float_vector_values::{FloatVectorValue, FloatVectorValues};
use crate::core::index::knn_vector_values::{BitsImpl, DocIndexIterator, KnnVectorValues};
use crate::core::index::merge_state::SharedDocMap;
use crate::core::index::merge_state::{DocMap as MergeDocMap, MergeState, MergeStateDocMap};
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::sorter::DocMap;
use crate::core::index::vector_encoding::VectorEncoding;
use crate::core::index::{DocIDMerger, DocIDMergerEnum, Sub, SubBase, of};
use crate::core::search::doc_id_set::DocIdSet;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::dummy::dummy_vector_scorer::DummyVectorScorer;
use crate::core::store::IndexOutput;
use crate::core::store::directory::Directory;
use crate::core::store::memory_segment_index_input::MappedFloatVectors;
use crate::core::util::TryIntoInt;
use crate::core::util::accountable::Accountable;
use crate::core::util::bits::Bits;
use crate::core::util::close::Closeable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::info_stream::InfoStream;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

pub trait KnnVectorsWriter<O>: Accountable + Closeable
where
  O: IndexOutput,
{
  /// Adds a new field for indexing.
  fn add_field<D1, D2>(
    &mut self,
    _write_state: &SegmentWriteState<D1>,
    _segment_info: &SegmentInfo<D2>,
    _field_info: Arc<FieldInfo>,
  ) -> Result<usize>
  where
    D1: Directory<IndexOutput = O>,
  {
    Err(LuceneError::unsupported_operation(""))
  }

  /// Flushes all buffered data on disk.
  fn flush<DM>(&mut self, _max_doc: i32, _sort_map: Option<&DM>) -> Result<()>
  where
    DM: DocMap,
  {
    Err(LuceneError::unsupported_operation(""))
  }

  fn merge_one_field<D1, D2, CR>(
    &mut self,
    field_info: &Arc<FieldInfo>,
    merge_state: &MergeState<'_, D1, CR>,
    segment_write_state: &SegmentWriteState<&D2>,
  ) -> Result<()>
  where
    D2: Directory<IndexOutput = O>,
    CR: CodecReader,
  {
    match field_info.get_vector_encoding() {
      VectorEncoding::BYTE(_) => {
        let field_vectors_writer_idx = self.add_field(
          segment_write_state,
          merge_state.segment_info,
          field_info.clone(),
        )?;
        let merged_bytes = merge_byte_vector_values(field_info.as_ref(), merge_state)?;
        let mut iter = merged_bytes.iterator()?;
        let mut doc = iter.next_doc()?;
        while doc != NO_MORE_DOCS {
          let ord: usize = iter.index()?.try_convert()?;
          iter.with_vector_value(ord, |value| {
            self.add_value(doc, value.as_ref(), field_vectors_writer_idx)
          })?;
          doc = iter.next_doc()?;
        }
      },
      VectorEncoding::FLOAT32(_) => {
        let field_vectors_writer_idx = self.add_field(
          segment_write_state,
          merge_state.segment_info,
          field_info.clone(),
        )?;
        let merged_floats = merge_float_vector_values(field_info.as_ref(), merge_state)?;
        let mut iter = merged_floats.iterator()?;
        let mut doc = iter.next_doc()?;
        while doc != NO_MORE_DOCS {
          let ord: usize = iter.index()?.try_convert()?;
          iter.with_vector_value(ord, |value| {
            self.add_value(doc, value.as_ref(), field_vectors_writer_idx)
          })?;
          doc = iter.next_doc()?;
        }
      },
    }
    Ok(())
  }

  fn finish(&mut self) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn merge<D1, D2, CR>(
    &mut self,
    merge_state: &MergeState<'_, D1, CR>,
    segment_write_state: &SegmentWriteState<&D2>,
  ) -> Result<i32>
  where
    D2: Directory<IndexOutput = O>,
    CR: CodecReader,
  {
    for (i, reader) in merge_state.knn_vectors_readers.iter().enumerate() {
      debug_assert!(reader.is_some() || !merge_state.field_infos[i].has_vector_values());
      if let Some(reader) = reader {
        reader.check_integrity()?;
      }
    }

    for field_info in merge_state.merge_field_infos.iter() {
      if field_info.has_vector_values() {
        if merge_state.info_stream.is_enabled("VV") {
          merge_state
            .info_stream
            .message("VV", &format!("merging {}", merge_state.segment_info))?;
        }

        self.merge_one_field(field_info, merge_state, segment_write_state)?;

        if merge_state.info_stream.is_enabled("VV") {
          merge_state
            .info_stream
            .message("VV", &format!("merge done {}", merge_state.segment_info))?;
        }
      }
    }
    self.finish_merge(merge_state)?;
    self.finish()?;
    merge_state.segment_info.max_doc()
  }
  fn finish_merge<D, CR>(&self, merge_state: &MergeState<'_, D, CR>) -> Result<()>
  where
    CR: CodecReader,
  {
    for reader in merge_state.knn_vectors_readers.iter().flatten() {
      reader.finish_merge()?;
    }
    Ok(())
  }

  fn add_value<V: VectorInput + ?Sized>(
    &mut self,
    _doc_id: i32,
    _vector_value: &V,
    _field_vectors_writers_idx: usize,
  ) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }
}

pub enum KnnVectorsWriterEnum2<A, B> {
  A(A),
  B(B),
}

impl<A, B> Closeable for KnnVectorsWriterEnum2<A, B>
where
  A: Closeable,
  B: Closeable,
{
  fn close(&mut self) -> Result<()> {
    match self {
      Self::A(inner) => inner.close(),
      Self::B(inner) => inner.close(),
    }
  }
}

impl<A, B> Accountable for KnnVectorsWriterEnum2<A, B>
where
  A: Accountable,
  B: Accountable,
{
  fn ram_bytes_used(&self) -> Result<i64> {
    match self {
      Self::A(inner) => inner.ram_bytes_used(),
      Self::B(inner) => inner.ram_bytes_used(),
    }
  }
}

impl<O, A, B> KnnVectorsWriter<O> for KnnVectorsWriterEnum2<A, B>
where
  O: IndexOutput,
  A: KnnVectorsWriter<O>,
  B: KnnVectorsWriter<O>,
{
  fn add_field<D1, D2>(
    &mut self,
    write_state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
    field_info: Arc<FieldInfo>,
  ) -> Result<usize>
  where
    D1: Directory<IndexOutput = O>,
  {
    match self {
      Self::A(inner) => inner.add_field(write_state, segment_info, field_info),
      Self::B(inner) => inner.add_field(write_state, segment_info, field_info),
    }
  }

  fn flush<DM>(&mut self, max_doc: i32, sort_map: Option<&DM>) -> Result<()>
  where
    DM: DocMap,
  {
    match self {
      Self::A(inner) => inner.flush(max_doc, sort_map),
      Self::B(inner) => inner.flush(max_doc, sort_map),
    }
  }

  fn merge_one_field<D1, D2, CR>(
    &mut self,
    field_info: &Arc<FieldInfo>,
    merge_state: &MergeState<'_, D1, CR>,
    segment_write_state: &SegmentWriteState<&D2>,
  ) -> Result<()>
  where
    D2: Directory<IndexOutput = O>,
    CR: CodecReader,
  {
    match self {
      Self::A(inner) => inner.merge_one_field(field_info, merge_state, segment_write_state),
      Self::B(inner) => inner.merge_one_field(field_info, merge_state, segment_write_state),
    }
  }

  fn finish(&mut self) -> Result<()> {
    match self {
      Self::A(inner) => inner.finish(),
      Self::B(inner) => inner.finish(),
    }
  }

  fn merge<D1, D2, CR>(
    &mut self,
    merge_state: &MergeState<'_, D1, CR>,
    segment_write_state: &SegmentWriteState<&D2>,
  ) -> Result<i32>
  where
    D2: Directory<IndexOutput = O>,
    CR: CodecReader,
  {
    match self {
      Self::A(inner) => inner.merge(merge_state, segment_write_state),
      Self::B(inner) => inner.merge(merge_state, segment_write_state),
    }
  }

  fn finish_merge<D, CR>(&self, merge_state: &MergeState<'_, D, CR>) -> Result<()>
  where
    CR: CodecReader,
  {
    match self {
      Self::A(inner) => inner.finish_merge(merge_state),
      Self::B(inner) => inner.finish_merge(merge_state),
    }
  }

  fn add_value<V: VectorInput + ?Sized>(
    &mut self,
    doc_id: i32,
    vector_value: &V,
    field_vectors_writers_idx: usize,
  ) -> Result<()> {
    match self {
      Self::A(inner) => inner.add_value(doc_id, vector_value, field_vectors_writers_idx),
      Self::B(inner) => inner.add_value(doc_id, vector_value, field_vectors_writers_idx),
    }
  }
}

/// Given old doc ids and an id mapping, maps old ordinal to new ordinal. Note: this method return
/// nothing and output are written to parameters
///
/// # Arguments
/// * `old_doc_ids` - the old or current document ordinals.
/// * `sort_map` - the document sorting map for how to make the new ordinals. Must not be None.
/// * `old2new_ord` - maps from old ord to new ord
/// * `new2old_ord` - maps from new ord to old ord
/// * `new_docs_with_field` - set of new doc ids which has the value
pub fn map_old_ord_to_new_ord<DM>(
  old_doc_ids: &DocsWithFieldSet,
  sort_map: &DM,
  mut old2new_ord: Option<&mut [usize]>,
  mut new2old_ord: Option<&mut [usize]>,
  mut new_docs_with_field: Option<&mut DocsWithFieldSet>,
) -> Result<()>
where
  DM: DocMap,
{
  debug_assert!(old2new_ord.is_some() || new2old_ord.is_some() || new_docs_with_field.is_some());
  let value_count = old_doc_ids.cardinality() as usize;

  debug_assert!({
    if let Some(ref arr) = old2new_ord {
      arr.len() == value_count
    } else {
      true
    }
  });
  debug_assert!({
    if let Some(ref arr) = new2old_ord {
      arr.len() == value_count
    } else {
      true
    }
  });

  let mut new_id_to_old_ord = HashMap::with_capacity(value_count);

  let mut iterator = old_doc_ids.iterator()?;
  let mut new_doc_ids = vec![0; value_count];

  let mut old_ord = 0;

  let mut old_doc_id = iterator.next_doc()?;
  while old_doc_id != NO_MORE_DOCS {
    let new_id = sort_map.old_to_new(old_doc_id)?;
    new_id_to_old_ord.insert(new_id, old_ord);
    new_doc_ids[old_ord] = new_id;
    old_ord += 1;

    old_doc_id = iterator.next_doc()?;
  }

  new_doc_ids.sort_unstable();

  for (new_ord, &new_doc_id) in new_doc_ids.iter().enumerate() {
    let curr_old_ord = *new_id_to_old_ord
      .get(&new_doc_id)
      .ok_or_else(|| LuceneError::illegal_state("missing mapping for new_doc_id"))?;

    if let Some(arr) = old2new_ord.as_mut() {
      arr[curr_old_ord] = new_ord;
    }

    if let Some(arr) = new2old_ord.as_mut() {
      arr[new_ord] = curr_old_ord;
    }

    if let Some(set) = new_docs_with_field.as_mut() {
      set.add(new_doc_id)?;
    }
  }

  Ok(())
}

pub struct FloatVectorValuesSub<F, DM>
where
  F: FloatVectorValues,
{
  values: F,
  mapped: Option<MappedFloatVectors>,
  iterator: <F as KnnVectorValues>::DocIndexIterator,
  doc_map: DM,
}
impl<F, DM> FloatVectorValuesSub<F, DM>
where
  F: FloatVectorValues,
{
  fn new(values: F, doc_map: DM) -> Result<Self> {
    let iterator = values.iterator()?;
    let mapped = values.mapped_float_vectors();
    Ok(Self {
      values,
      mapped,
      iterator,
      doc_map,
    })
  }
  fn index(&self) -> Result<i32> {
    self.iterator.index()
  }
}
impl<F, DM> SubBase for FloatVectorValuesSub<F, DM>
where
  F: FloatVectorValues,
  DM: MergeDocMap,
{
  fn next_doc(&mut self) -> Result<i32> {
    self.iterator.next_doc()
  }

  type DocMap = DM;

  fn get_doc_map(&self) -> Result<&Self::DocMap> {
    Ok(&self.doc_map)
  }
}
pub struct ByteVectorValuesSub<B, DM>
where
  B: ByteVectorValues,
{
  values: B,
  iterator: <B as KnnVectorValues>::DocIndexIterator,
  doc_map: DM,
}

impl<B, DM> ByteVectorValuesSub<B, DM>
where
  B: ByteVectorValues,
{
  fn new(values: B, doc_map: DM) -> Result<Self> {
    let iterator = values.iterator()?;
    Ok(Self {
      values,
      iterator,
      doc_map,
    })
  }

  fn index(&self) -> Result<i32> {
    self.iterator.index()
  }
}

impl<B, DM> SubBase for ByteVectorValuesSub<B, DM>
where
  B: ByteVectorValues,
  DM: MergeDocMap,
{
  type DocMap = DM;

  fn next_doc(&mut self) -> Result<i32> {
    self.iterator.next_doc()
  }

  fn get_doc_map(&self) -> Result<&Self::DocMap> {
    Ok(&self.doc_map)
  }
}

pub(crate) fn validate_field_encoding(
  field_info: &FieldInfo,
  expected: VectorEncoding,
) -> Result<()> {
  debug_assert!(field_info.has_vector_values());

  let field_encoding = *field_info.get_vector_encoding();
  if field_encoding != expected {
    return Err(LuceneError::unsupported_operation(format!(
      "Cannot merge vectors encoded as [{field_encoding}] as {expected}"
    )));
  }

  Ok(())
}

pub(crate) fn has_vector_values(field_infos: &FieldInfos, field_name: &str) -> Result<bool> {
  if !field_infos.has_vector_values() {
    return Ok(false);
  }

  Ok(
    field_infos
      .field_info_by_name(field_name)?
      .is_some_and(|info| info.has_vector_values()),
  )
}

struct MergedFloat32VectorValuesState<F, DM>
where
  F: FloatVectorValues,
{
  doc_id: i32,
  last_ord: i32,
  current: Option<usize>,
  doc_id_merger: DocIDMergerEnum<FloatVectorValuesSub<F, DM>>,
}

pub struct MergedFloat32VectorValues<F, DM>
where
  F: FloatVectorValues,
{
  state: Rc<RefCell<MergedFloat32VectorValuesState<F, DM>>>,
  size: usize,
  dimension: usize,
}

impl<F, DM> MergedFloat32VectorValues<F, DM>
where
  F: FloatVectorValues,
  DM: MergeDocMap,
{
  pub(crate) fn new<Dir, CR>(
    subs: Vec<Sub<FloatVectorValuesSub<F, DM>>>,
    merge_state: &MergeState<'_, Dir, CR>,
  ) -> Result<Self>
  where
    CR: CodecReader,
  {
    let dimension = match subs.first() {
      Some(v) => v.sub.values.dimension(),
      None => return Err(LuceneError::illegal_state("no sub-vectors to merge")),
    };
    let size = subs.iter().map(|sub| sub.sub.values.size()).sum();
    let doc_id_merger = of(subs, merge_state.needs_index_sort)?;
    Ok(Self {
      state: Rc::new(RefCell::new(MergedFloat32VectorValuesState {
        doc_id: -1,
        last_ord: -1,
        current: None,
        doc_id_merger,
      })),
      size,
      dimension,
    })
  }
}

impl<F, DM> KnnVectorValues for MergedFloat32VectorValues<F, DM>
where
  F: FloatVectorValues,
  DM: MergeDocMap,
{
  fn dimension(&self) -> usize {
    self.dimension
  }

  fn size(&self) -> usize {
    self.size
  }

  fn ord_to_doc(&self, _ord: usize) -> Result<usize> {
    Err(LuceneError::unsupported_operation(""))
  }

  type KnnVectorValues = DummyKnnVectorsWriter;

  fn get_encoding(&self) -> VectorEncoding {
    FloatVectorValues::get_encoding(self)
  }

  type Bits<'a, B>
    = BitsImpl<B, &'a Self>
  where
    B: Bits,
    Self: 'a;

  fn get_accept_ords<'a, B>(&'a self, accept_docs: Option<B>) -> Option<Self::Bits<'a, B>>
  where
    B: Bits,
  {
    self.default_get_accept_ords(accept_docs)
  }

  type DocIndexIterator = MergedFloat32VectorValuesIterator<F, DM>;

  fn iterator(&self) -> Result<Self::DocIndexIterator> {
    Ok(MergedFloat32VectorValuesIterator {
      state: Rc::clone(&self.state),
      index: -1,
      size: self.size,
    })
  }
}

impl<F, DM> FloatVectorValues for MergedFloat32VectorValues<F, DM>
where
  F: FloatVectorValues,
  DM: MergeDocMap,
{
  type VectorValue<'v>
    = FloatVectorValue<'v>
  where
    Self: 'v;

  fn vector_value(&self, ord: usize) -> Result<Self::VectorValue<'_>> {
    let state = self.state.borrow();
    let ord: i32 = ord.try_convert()?;
    if ord != state.last_ord {
      return Err(LuceneError::illegal_state(format!(
        "only supports forward iteration with a single iterator: ord={ord}, lastOrd={}",
        state.last_ord
      )));
    }
    let current = state
      .current
      .ok_or_else(|| LuceneError::illegal_state("missing current vector sub"))?;
    let current_sub = &state.doc_id_merger.get_subs()[current].sub;
    let index: usize = current_sub.index()?.try_convert()?;
    Ok(
      Into::<FloatVectorValue<'_>>::into(current_sub.values.vector_value(index)?)
        .into_owned()
        .into(),
    )
  }

  type FloatVectorValues = DummyFloatVectorValues;

  fn float_copy(&self) -> Result<Option<Self::FloatVectorValues>> {
    Err(LuceneError::unsupported_operation(""))
  }

  type VectorScorer = DummyVectorScorer;

  fn scorer<'a, TV>(&self, _target: TV) -> Result<Option<Self::VectorScorer>>
  where
    TV: Into<std::borrow::Cow<'a, [f32]>>,
  {
    Err(LuceneError::unsupported_operation(""))
  }
}

pub struct MergedFloat32VectorValuesIterator<F, DM>
where
  F: FloatVectorValues,
{
  state: Rc<RefCell<MergedFloat32VectorValuesState<F, DM>>>,
  index: i32,
  size: usize,
}

impl<F, DM> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for MergedFloat32VectorValuesIterator<F, DM>
where
  F: FloatVectorValues,
  DM: MergeDocMap,
{
}
impl<F, DM> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for MergedFloat32VectorValuesIterator<F, DM>
where
  F: FloatVectorValues,
  DM: MergeDocMap,
{
}

impl<F, DM> DocIdSetIterator for MergedFloat32VectorValuesIterator<F, DM>
where
  F: FloatVectorValues,
  DM: MergeDocMap,
{
  fn doc_id(&self) -> i32 {
    self.state.borrow().doc_id
  }

  fn next_doc(&mut self) -> Result<i32> {
    let mut state = self.state.borrow_mut();
    state.current = state.doc_id_merger.next()?;
    match state.current {
      Some(current) => {
        state.doc_id = state.doc_id_merger.get_subs()[current].mapped_doc_id;
        state.last_ord += 1;
        self.index += 1;
        Ok(state.doc_id)
      },
      None => {
        state.doc_id = NO_MORE_DOCS;
        self.index = NO_MORE_DOCS;
        Ok(NO_MORE_DOCS)
      },
    }
  }

  fn advance(&mut self, _target: i32) -> Result<i32> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn cost(&self) -> Result<i64> {
    self.size.try_convert()
  }
}

impl<F, DM> DocIndexIterator for MergedFloat32VectorValuesIterator<F, DM>
where
  F: FloatVectorValues,
  DM: MergeDocMap,
{
  fn index(&self) -> Result<i32> {
    Ok(self.index)
  }
}
impl<F, DM> MergedFloat32VectorValuesIterator<F, DM>
where
  F: FloatVectorValues,
  DM: MergeDocMap,
{
  pub(crate) fn with_vector_value<R>(
    &self,
    ord: usize,
    consume: impl FnOnce(FloatVectorValue<'_>) -> Result<R>,
  ) -> Result<R> {
    let state = self.state.borrow();
    let ord: i32 = ord.try_convert()?;
    if ord != state.last_ord {
      return Err(LuceneError::illegal_state(format!(
        "only supports forward iteration with a single iterator: ord={ord}, lastOrd={}",
        state.last_ord
      )));
    }

    let current = state
      .current
      .ok_or_else(|| LuceneError::illegal_state("missing current vector sub"))?;
    let current_sub = &state.doc_id_merger.get_subs()[current].sub;
    let index: usize = current_sub.index()?.try_convert()?;
    if let Some(mapped) = &current_sub.mapped
      && let Some(vectors) = mapped.as_slice()
      && let Some(start) = index.checked_mul(mapped.dimension())
      && let Some(vector) = vectors
        .get(start..)
        .and_then(|tail| tail.get(..mapped.dimension()))
    {
      return consume(vector.into());
    }
    let value: FloatVectorValue<'_> = current_sub.values.vector_value(index)?.into();
    consume(value)
  }
}

struct MergedByteVectorValuesState<B, DM>
where
  B: ByteVectorValues,
{
  doc_id: i32,
  last_ord: i32,
  current: Option<usize>,
  doc_id_merger: DocIDMergerEnum<ByteVectorValuesSub<B, DM>>,
}

pub struct MergedByteVectorValues<B, DM>
where
  B: ByteVectorValues,
{
  state: Rc<RefCell<MergedByteVectorValuesState<B, DM>>>,
  size: usize,
  dimension: usize,
}

impl<B, DM> MergedByteVectorValues<B, DM>
where
  B: ByteVectorValues,
  DM: MergeDocMap,
{
  pub(crate) fn new<Dir, CR>(
    subs: Vec<Sub<ByteVectorValuesSub<B, DM>>>,
    merge_state: &MergeState<'_, Dir, CR>,
  ) -> Result<Self>
  where
    CR: CodecReader,
  {
    let dimension = match subs.first() {
      Some(v) => v.sub.values.dimension(),
      None => return Err(LuceneError::illegal_state("no sub-vectors to merge")),
    };
    let size = subs.iter().map(|sub| sub.sub.values.size()).sum();
    let doc_id_merger = of(subs, merge_state.needs_index_sort)?;
    Ok(Self {
      state: Rc::new(RefCell::new(MergedByteVectorValuesState {
        doc_id: -1,
        last_ord: -1,
        current: None,
        doc_id_merger,
      })),
      size,
      dimension,
    })
  }
}

impl<B, DM> KnnVectorValues for MergedByteVectorValues<B, DM>
where
  B: ByteVectorValues,
  DM: MergeDocMap,
{
  fn dimension(&self) -> usize {
    self.dimension
  }

  fn size(&self) -> usize {
    self.size
  }

  fn ord_to_doc(&self, _ord: usize) -> Result<usize> {
    Err(LuceneError::unsupported_operation(""))
  }

  type KnnVectorValues = DummyKnnVectorsWriter;

  fn get_encoding(&self) -> VectorEncoding {
    ByteVectorValues::get_encoding(self)
  }

  type Bits<'a, B1>
    = BitsImpl<B1, &'a Self>
  where
    B1: Bits,
    Self: 'a;

  fn get_accept_ords<'a, B1>(&'a self, accept_docs: Option<B1>) -> Option<Self::Bits<'a, B1>>
  where
    B1: Bits,
  {
    self.default_get_accept_ords(accept_docs)
  }

  type DocIndexIterator = MergedByteVectorValuesIterator<B, DM>;

  fn iterator(&self) -> Result<Self::DocIndexIterator> {
    Ok(MergedByteVectorValuesIterator {
      state: Rc::clone(&self.state),
      index: -1,
      size: self.size,
    })
  }
}

impl<B, DM> ByteVectorValues for MergedByteVectorValues<B, DM>
where
  B: ByteVectorValues,
  DM: MergeDocMap,
{
  type VectorValue<'v>
    = ByteVectorValue<'v>
  where
    Self: 'v;

  fn vector_value(&self, ord: usize) -> Result<Self::VectorValue<'_>> {
    let mut state = self.state.borrow_mut();
    let ord: i32 = ord.try_convert()?;
    if ord != state.last_ord + 1 {
      return Err(LuceneError::illegal_state(format!(
        "only supports forward iteration: ord={ord}, lastOrd={}",
        state.last_ord
      )));
    }
    state.last_ord = ord;
    let current = state
      .current
      .ok_or_else(|| LuceneError::illegal_state("missing current vector sub"))?;
    let current_sub = &state.doc_id_merger.get_subs()[current].sub;
    let index: usize = current_sub.index()?.try_convert()?;
    Ok(
      Into::<ByteVectorValue<'_>>::into(current_sub.values.vector_value(index)?)
        .into_owned()
        .into(),
    )
  }

  type ByteVectorValues = DummyByteVectorValues;

  fn byte_copy(&self) -> Result<Option<Self::ByteVectorValues>> {
    Err(LuceneError::unsupported_operation(""))
  }

  type VectorScorer = DummyVectorScorer;

  fn scorer<TV>(&self, _query: TV) -> Result<Option<Self::VectorScorer>>
  where
    TV: Into<Vec<u8>>,
  {
    Err(LuceneError::unsupported_operation(""))
  }
}

pub struct MergedByteVectorValuesIterator<B, DM>
where
  B: ByteVectorValues,
{
  state: Rc<RefCell<MergedByteVectorValuesState<B, DM>>>,
  index: i32,
  size: usize,
}

impl<B, DM> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for MergedByteVectorValuesIterator<B, DM>
where
  B: ByteVectorValues,
  DM: MergeDocMap,
{
}
impl<B, DM> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for MergedByteVectorValuesIterator<B, DM>
where
  B: ByteVectorValues,
  DM: MergeDocMap,
{
}

impl<B, DM> DocIdSetIterator for MergedByteVectorValuesIterator<B, DM>
where
  B: ByteVectorValues,
  DM: MergeDocMap,
{
  fn doc_id(&self) -> i32 {
    self.state.borrow().doc_id
  }

  fn next_doc(&mut self) -> Result<i32> {
    let mut state = self.state.borrow_mut();
    state.current = state.doc_id_merger.next()?;
    match state.current {
      Some(current) => {
        state.doc_id = state.doc_id_merger.get_subs()[current].mapped_doc_id;
        self.index += 1;
        Ok(state.doc_id)
      },
      None => {
        state.doc_id = NO_MORE_DOCS;
        self.index = NO_MORE_DOCS;
        Ok(NO_MORE_DOCS)
      },
    }
  }

  fn advance(&mut self, _target: i32) -> Result<i32> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn cost(&self) -> Result<i64> {
    self.size.try_convert()
  }
}

impl<B, DM> DocIndexIterator for MergedByteVectorValuesIterator<B, DM>
where
  B: ByteVectorValues,
  DM: MergeDocMap,
{
  fn index(&self) -> Result<i32> {
    Ok(self.index)
  }
}

impl<B, DM> MergedByteVectorValuesIterator<B, DM>
where
  B: ByteVectorValues,
  DM: MergeDocMap,
{
  pub(crate) fn with_vector_value<R>(
    &self,
    ord: usize,
    consume: impl FnOnce(ByteVectorValue<'_>) -> Result<R>,
  ) -> Result<R> {
    let mut state = self.state.borrow_mut();
    let ord: i32 = ord.try_convert()?;
    if ord != state.last_ord + 1 {
      return Err(LuceneError::illegal_state(format!(
        "only supports forward iteration: ord={ord}, lastOrd={}",
        state.last_ord
      )));
    }
    state.last_ord = ord;

    let current = state
      .current
      .ok_or_else(|| LuceneError::illegal_state("missing current vector sub"))?;
    let current_sub = &state.doc_id_merger.get_subs()[current].sub;
    let index: usize = current_sub.index()?.try_convert()?;
    let value: ByteVectorValue<'_> = current_sub.values.vector_value(index)?.into();
    consume(value)
  }
}

fn merge_vector_values<D, CR, V, S, VSupplier, NewSub>(
  merge_state: &MergeState<'_, D, CR>,
  merging_field: &FieldInfo,
  mut values_supplier: VSupplier,
  mut new_sub: NewSub,
) -> Result<Vec<Sub<S>>>
where
  CR: CodecReader,
  V: KnnVectorValues,
  S: SubBase<DocMap = SharedDocMap<MergeStateDocMap<CR>>>,
  VSupplier: FnMut(&CRKnnVectorReader<CR>, &str) -> Result<V>,
  NewSub: FnMut(SharedDocMap<MergeStateDocMap<CR>>, V) -> Result<S>,
{
  let mut subs = Vec::with_capacity(merge_state.knn_vectors_readers.len());
  for i in 0..merge_state.knn_vectors_readers.len() {
    let source_field_info = &merge_state.field_infos[i];
    if !has_vector_values(source_field_info, &merging_field.name)? {
      continue;
    }

    if let Some(knn_vectors_reader) = merge_state.knn_vectors_readers[i].as_ref() {
      let values = values_supplier(knn_vectors_reader, &merging_field.name)?;
      subs.push(Sub::new(new_sub(merge_state.doc_map(i), values)?));
    }
  }
  Ok(subs)
}

#[allow(clippy::type_complexity)]
pub(crate) fn merge_float_vector_values<D, CR>(
  field_info: &FieldInfo,
  merge_state: &MergeState<'_, D, CR>,
) -> Result<
  MergedFloat32VectorValues<
    <CRKnnVectorReader<CR> as KnnVectorsReader>::FloatVectorValues,
    SharedDocMap<MergeStateDocMap<CR>>,
  >,
>
where
  CR: CodecReader,
{
  validate_field_encoding(field_info, VectorEncoding::FLOAT32(4))?;
  MergedFloat32VectorValues::new(
    merge_vector_values(
      merge_state,
      field_info,
      |knn_vectors_reader, field| knn_vectors_reader.get_float_vector_values(field),
      |doc_map, values| FloatVectorValuesSub::new(values, doc_map),
    )?,
    merge_state,
  )
}

#[allow(clippy::type_complexity)]
pub(crate) fn merge_byte_vector_values<D, CR>(
  field_info: &FieldInfo,
  merge_state: &MergeState<'_, D, CR>,
) -> Result<
  MergedByteVectorValues<
    <CRKnnVectorReader<CR> as KnnVectorsReader>::ByteVectorValues,
    SharedDocMap<MergeStateDocMap<CR>>,
  >,
>
where
  CR: CodecReader,
{
  validate_field_encoding(field_info, VectorEncoding::BYTE(1))?;
  MergedByteVectorValues::new(
    merge_vector_values(
      merge_state,
      field_info,
      |knn_vectors_reader, field| knn_vectors_reader.get_byte_vector_values(field),
      |doc_map, values| ByteVectorValuesSub::new(values, doc_map),
    )?,
    merge_state,
  )
}
