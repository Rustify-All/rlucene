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
use crate::core::codecs::knn_field_vectors_writer::VectorValueEnum;
use crate::core::codecs::knn_vectors_reader::KnnVectorsReader;
use crate::core::index::byte_vector_values::ByteVectorValues;
use crate::core::index::codec_reader::CRKnnVectorReader;
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::docs_with_field_set::DocsWithFieldSet;
use crate::core::index::dummy::dummy_byte_vector_values::DummyByteVectorValues;
use crate::core::index::dummy::dummy_float_vector_values::DummyFloatVectorValues;
use crate::core::index::dummy::dummy_knn_vector_values::DummyKnnVectorsWriter;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::float_vector_values::FloatVectorValues;
use crate::core::index::knn_vector_values::{BitsImpl1, DocIndexIterator, KnnVectorValues};
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
use crate::core::util::TryIntoInt;
use crate::core::util::accountable::Accountable;
use crate::core::util::bits::Bits;
use crate::core::util::close::Closeable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::info_stream::InfoStream;
use parking_lot::Mutex;
use std::borrow::Cow;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub trait KnnVectorsWriter: Accountable + Closeable {
  type IndexOutput: IndexOutput;

  /// Adds a new field for indexing.
  fn add_field<D1, D2>(
    &mut self,
    _write_state: &SegmentWriteState<D1>,
    _segment_info: &SegmentInfo<D2>,
    _field_info: Arc<FieldInfo>,
  ) -> Result<usize>
  where
    D1: Directory<IndexOutput = Self::IndexOutput>,
    D2: Directory,
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
    D1: Directory,
    D2: Directory<IndexOutput = Self::IndexOutput>,
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
          let vector_value = iter.vector_value(ord)?;
          self.add_value(doc, &vector_value, field_vectors_writer_idx)?;
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
          let vector_value = iter.vector_value(ord)?;
          self.add_value(doc, &vector_value, field_vectors_writer_idx)?;
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
    D1: Directory,
    D2: Directory<IndexOutput = Self::IndexOutput>,
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
    D: Directory,
    CR: CodecReader,
  {
    for reader in merge_state.knn_vectors_readers.iter().flatten() {
      reader.finish_merge()?;
    }
    Ok(())
  }

  fn add_value(
    &mut self,
    _doc_id: i32,
    _vector_value: &VectorValueEnum,
    _field_vectors_writers_idx: usize,
  ) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
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

  debug_assert!({
    if let Some(ref arr) = old2new_ord {
      arr.len() == old_doc_ids.cardinality() as usize
    } else {
      true
    }
  });
  debug_assert!({
    if let Some(ref arr) = new2old_ord {
      arr.len() == old_doc_ids.cardinality() as usize
    } else {
      true
    }
  });

  let mut new_id_to_old_ord = HashMap::new();

  let mut iterator = old_doc_ids.iterator()?;
  let mut new_doc_ids = vec![0; old_doc_ids.cardinality() as usize];

  let mut old_ord = 0;

  let mut old_doc_id = iterator.next_doc()?;
  while old_doc_id != NO_MORE_DOCS {
    let new_id = sort_map.old_to_new(old_doc_id)? as usize;
    new_id_to_old_ord.insert(new_id, old_ord);
    new_doc_ids[old_ord] = new_id;
    old_ord += 1;

    old_doc_id = iterator.next_doc()?;
  }

  new_doc_ids.sort();

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
      set.add(new_doc_id as i32)?;
    }
  }

  Ok(())
}

pub struct FloatVectorValuesSub<F, DM>
where
  F: FloatVectorValues,
  DM: MergeDocMap,
{
  values: F,
  iterator: <F as KnnVectorValues>::DocIndexIterator,
  doc_map: DM,
}
impl<F, DM> FloatVectorValuesSub<F, DM>
where
  F: FloatVectorValues,
  DM: MergeDocMap,
{
  fn new(values: F, doc_map: DM) -> Result<Self> {
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
  DM: MergeDocMap,
{
  values: B,
  iterator: <B as KnnVectorValues>::DocIndexIterator,
  doc_map: DM,
}

impl<B, DM> ByteVectorValuesSub<B, DM>
where
  B: ByteVectorValues,
  DM: MergeDocMap,
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
  DM: MergeDocMap,
{
  doc_id: i32,
  last_ord: i32,
  current: Option<usize>,
  doc_id_merger: DocIDMergerEnum<FloatVectorValuesSub<F, DM>>,
}

pub struct MergedFloat32VectorValues<F, DM>
where
  F: FloatVectorValues,
  DM: MergeDocMap,
{
  // for easy taken
  state: Mutex<Option<MergedFloat32VectorValuesState<F, DM>>>,
  size: usize,
  dimension: usize,
  iter_called: AtomicBool,
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
    Dir: Directory,
    CR: CodecReader,
  {
    let dimension = match subs.first() {
      Some(v) => v.sub.values.dimension(),
      None => return Err(LuceneError::illegal_state("no sub-vectors to merge")),
    };
    let size = subs.iter().map(|sub| sub.sub.values.size()).sum();
    let doc_id_merger = of(subs, merge_state.needs_index_sort)?;
    Ok(Self {
      state: Mutex::new(Some(MergedFloat32VectorValuesState {
        doc_id: -1,
        last_ord: -1,
        current: None,
        doc_id_merger,
      })),
      size,
      dimension,
      iter_called: AtomicBool::new(false),
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
    = BitsImpl1<B>
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
    if self
      .iter_called
      .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
      .is_err()
    {
      return Err(LuceneError::illegal_state(
        "iterator() can only be called once on MergedFloat32VectorValues",
      ));
    }
    let state = self.state.lock().take().ok_or_else(|| {
      LuceneError::illegal_state("MergedFloat32VectorValues iterator state missing")
    })?;

    Ok(MergedFloat32VectorValuesIterator {
      state,
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
  fn vector_value(&self, _ord: usize) -> Result<std::borrow::Cow<'_, VectorValueEnum>> {
    Err(LuceneError::unsupported_operation(
      "should use iterator()'s vector_value()",
    ))
  }

  type FloatVectorValues = DummyFloatVectorValues;

  fn float_copy(&self) -> Result<Option<Self::FloatVectorValues>> {
    Err(LuceneError::unsupported_operation(""))
  }

  type VectorScorer = DummyVectorScorer;

  fn scorer(&self, _target: Vec<f32>) -> Result<Option<Self::VectorScorer>> {
    Err(LuceneError::unsupported_operation(""))
  }
}

pub struct MergedFloat32VectorValuesIterator<F, DM>
where
  F: FloatVectorValues,
  DM: MergeDocMap,
{
  state: MergedFloat32VectorValuesState<F, DM>,
  index: i32,
  size: usize,
}

impl<F, DM> DocIdSetIterator for MergedFloat32VectorValuesIterator<F, DM>
where
  F: FloatVectorValues,
  DM: MergeDocMap,
{
  fn doc_id(&self) -> i32 {
    self.state.doc_id
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.state.current = self.state.doc_id_merger.next()?;
    match self.state.current {
      Some(current) => {
        self.state.doc_id = self.state.doc_id_merger.get_subs()[current].mapped_doc_id;
        self.state.last_ord += 1;
        self.index += 1;
        Ok(self.state.doc_id)
      },
      None => {
        self.state.doc_id = NO_MORE_DOCS;
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
impl<F, DM> MergeVectorValues for MergedFloat32VectorValuesIterator<F, DM>
where
  F: FloatVectorValues,
  DM: MergeDocMap,
{
  fn vector_value(&self, ord: usize) -> Result<Cow<'_, VectorValueEnum>> {
    let ord: i32 = ord.try_convert()?;
    if ord != self.state.last_ord {
      return Err(LuceneError::illegal_state(format!(
        "only supports forward iteration with a single iterator: ord={ord}, lastOrd={}",
        self.state.last_ord
      )));
    }

    let current = self
      .state
      .current
      .ok_or_else(|| LuceneError::illegal_state("missing current vector sub"))?;
    let current_sub = &self.state.doc_id_merger.get_subs()[current].sub;
    let index: usize = current_sub.index()?.try_convert()?;
    current_sub.values.vector_value(index)
  }
}

struct MergedByteVectorValuesState<B, DM>
where
  B: ByteVectorValues,
  DM: MergeDocMap,
{
  doc_id: i32,
  last_ord: i32,
  current: Option<usize>,
  doc_id_merger: DocIDMergerEnum<ByteVectorValuesSub<B, DM>>,
}

pub struct MergedByteVectorValues<B, DM>
where
  B: ByteVectorValues,
  DM: MergeDocMap,
{
  state: Mutex<Option<MergedByteVectorValuesState<B, DM>>>,
  size: usize,
  dimension: usize,
  iter_called: AtomicBool,
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
    Dir: Directory,
    CR: CodecReader,
  {
    let dimension = match subs.first() {
      Some(v) => v.sub.values.dimension(),
      None => return Err(LuceneError::illegal_state("no sub-vectors to merge")),
    };
    let size = subs.iter().map(|sub| sub.sub.values.size()).sum();
    let doc_id_merger = of(subs, merge_state.needs_index_sort)?;
    Ok(Self {
      state: Mutex::new(Some(MergedByteVectorValuesState {
        doc_id: -1,
        last_ord: -1,
        current: None,
        doc_id_merger,
      })),
      size,
      dimension,
      iter_called: AtomicBool::new(false),
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
    = BitsImpl1<B1>
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
    if self
      .iter_called
      .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
      .is_err()
    {
      return Err(LuceneError::illegal_state(
        "iterator() can only be called once on MergedByteVectorValues",
      ));
    }
    let state =
      self.state.lock().take().ok_or_else(|| {
        LuceneError::illegal_state("MergedByteVectorValues iterator state missing")
      })?;

    Ok(MergedByteVectorValuesIterator {
      state,
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
  fn vector_value(&self, _ord: usize) -> Result<Cow<'_, VectorValueEnum>> {
    Err(LuceneError::unsupported_operation(
      "should use iterator()'s vector_value()",
    ))
  }

  type ByteVectorValues = DummyByteVectorValues;

  fn byte_copy(&self) -> Result<Option<Self::ByteVectorValues>> {
    Err(LuceneError::unsupported_operation(""))
  }

  type VectorScorer = DummyVectorScorer;

  fn scorer(&self, _query: Vec<u8>) -> Result<Option<Self::VectorScorer>> {
    Err(LuceneError::unsupported_operation(""))
  }
}

pub struct MergedByteVectorValuesIterator<B, DM>
where
  B: ByteVectorValues,
  DM: MergeDocMap,
{
  state: MergedByteVectorValuesState<B, DM>,
  index: i32,
  size: usize,
}

impl<B, DM> DocIdSetIterator for MergedByteVectorValuesIterator<B, DM>
where
  B: ByteVectorValues,
  DM: MergeDocMap,
{
  fn doc_id(&self) -> i32 {
    self.state.doc_id
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.state.current = self.state.doc_id_merger.next()?;
    match self.state.current {
      Some(current) => {
        self.state.doc_id = self.state.doc_id_merger.get_subs()[current].mapped_doc_id;
        self.state.last_ord += 1;
        self.index += 1;
        Ok(self.state.doc_id)
      },
      None => {
        self.state.doc_id = NO_MORE_DOCS;
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

impl<B, DM> MergeVectorValues for MergedByteVectorValuesIterator<B, DM>
where
  B: ByteVectorValues,
  DM: MergeDocMap,
{
  fn vector_value(&self, ord: usize) -> Result<Cow<'_, VectorValueEnum>> {
    let ord: i32 = ord.try_convert()?;
    if ord != self.state.last_ord {
      return Err(LuceneError::illegal_state(format!(
        "only supports forward iteration with a single iterator: ord={ord}, lastOrd={}",
        self.state.last_ord
      )));
    }

    let current = self
      .state
      .current
      .ok_or_else(|| LuceneError::illegal_state("missing current vector sub"))?;
    let current_sub = &self.state.doc_id_merger.get_subs()[current].sub;
    let index: usize = current_sub.index()?.try_convert()?;
    current_sub.values.vector_value(index)
  }
}

pub(crate) trait MergeVectorValues {
  fn vector_value(&self, ord: usize) -> Result<std::borrow::Cow<'_, VectorValueEnum>>;
}

fn merge_vector_values<D, CR, V, S, VSupplier, NewSub>(
  merge_state: &MergeState<'_, D, CR>,
  merging_field: &FieldInfo,
  mut values_supplier: VSupplier,
  mut new_sub: NewSub,
) -> Result<Vec<Sub<S>>>
where
  D: Directory,
  CR: CodecReader,
  V: KnnVectorValues,
  S: SubBase<DocMap = Rc<MergeStateDocMap<CR>>>,
  VSupplier: FnMut(&CRKnnVectorReader<CR>, &str) -> Result<V>,
  NewSub: FnMut(Rc<MergeStateDocMap<CR>>, V) -> Result<S>,
{
  let mut subs = Vec::new();
  for i in 0..merge_state.knn_vectors_readers.len() {
    let source_field_info = &merge_state.field_infos[i];
    if !has_vector_values(source_field_info, &merging_field.name)? {
      continue;
    }

    if let Some(knn_vectors_reader) = merge_state.knn_vectors_readers[i].as_ref() {
      let values = values_supplier(knn_vectors_reader, &merging_field.name)?;
      subs.push(Sub::new(new_sub(merge_state.doc_maps[i].clone(), values)?));
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
    Rc<MergeStateDocMap<CR>>,
  >,
>
where
  D: Directory,
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
    Rc<MergeStateDocMap<CR>>,
  >,
>
where
  D: Directory,
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
