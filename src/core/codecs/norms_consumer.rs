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
use crate::core::codecs::norms_producer::NormsProducer;
use crate::core::index::codec_reader::{CRNormsProducer, CodecReader};
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::merge_state::SharedDocMap;
use crate::core::index::merge_state::{DocMap, MergeState, MergeStateDocMap};
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::{DocIDMerger, DocIDMergerEnum, Sub, SubBase, of};
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::sync::Arc;

/// Consumes normalization values.
///
/// Concrete implementations actually do *something* with the norms,
/// such as writing them into the index in a specific format.
///
/// # Lifecycle
///
/// 1. [`NormsConsumer`] is created by
///    [`NormsFormat::norms_consumer`](crate::core::codecs::norms_format::NormsFormat::norms_consumer).
/// 2. [`add_norms_field`](NormsConsumer::add_norms_field) is called for each
///    field with normalization values. The API is *pull*-based rather than
///    *push*-based; the implementation is free to iterate over the values
///    multiple times.
/// 3. After all fields are added, the consumer is closed.
pub trait NormsConsumer: Closeable {
  /// Writes normalization values for a field.
  ///
  /// # Arguments
  /// * `field` - Field metadata
  /// * `norms_producer` - Provides numeric norms for the field
  ///
  /// # Errors
  /// If an I/O error occurs during writing.
  fn add_norms_field<T>(&mut self, field: &Arc<FieldInfo>, norms_producer: &mut T) -> Result<()>
  where
    T: NormsProducer;
  /// Merges in the fields from the readers in `merge_state`.
  ///
  /// The default implementation calls
  /// [`merge_norms_field`](NormsConsumer::merge_norms_field) for each field,
  /// filling segments with missing norms for the field with zeros.
  ///
  /// Implementations can provide this method for more sophisticated merging.
  /// (e.g. bulk-byte copying).
  fn merge<D, CR>(&mut self, merge_state: &MergeState<D, CR>) -> Result<()>
  where
    CR: CodecReader,
  {
    for producer in merge_state.norms_producers.iter().flatten() {
      producer.check_integrity()?;
    }

    for field_info in merge_state.merge_field_infos.as_ref() {
      if field_info.has_norms() {
        self.merge_norms_field(field_info, merge_state)?;
      }
    }

    Ok(())
  }
  /// Merges the norms from `to_merge`.
  ///
  /// The default implementation calls
  /// [`add_norms_field`](NormsConsumer::add_norms_field), passing an iterator
  /// that merges and filters deleted documents on the fly.
  fn merge_norms_field<D, CR>(
    &mut self,
    merge_field_info: &Arc<FieldInfo>,
    merge_state: &MergeState<D, CR>,
  ) -> Result<()>
  where
    CR: CodecReader,
  {
    let mut norms_producer = NormsProducerMerge {
      merge_field_info,
      merge_state,
    };
    self.add_norms_field(merge_field_info, &mut norms_producer)?;
    Ok(())
  }
}

pub enum NormsConsumerEnum2<A, B> {
  A(A),
  B(B),
}

impl<A, B> Closeable for NormsConsumerEnum2<A, B>
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

impl<A, B> NormsConsumer for NormsConsumerEnum2<A, B>
where
  A: NormsConsumer,
  B: NormsConsumer,
{
  fn add_norms_field<T>(&mut self, field: &Arc<FieldInfo>, norms_producer: &mut T) -> Result<()>
  where
    T: NormsProducer,
  {
    match self {
      Self::A(inner) => inner.add_norms_field(field, norms_producer),
      Self::B(inner) => inner.add_norms_field(field, norms_producer),
    }
  }

  fn merge<D, CR>(&mut self, merge_state: &MergeState<D, CR>) -> Result<()>
  where
    CR: CodecReader,
  {
    match self {
      Self::A(inner) => inner.merge(merge_state),
      Self::B(inner) => inner.merge(merge_state),
    }
  }

  fn merge_norms_field<D, CR>(
    &mut self,
    merge_field_info: &Arc<FieldInfo>,
    merge_state: &MergeState<D, CR>,
  ) -> Result<()>
  where
    CR: CodecReader,
  {
    match self {
      Self::A(inner) => inner.merge_norms_field(merge_field_info, merge_state),
      Self::B(inner) => inner.merge_norms_field(merge_field_info, merge_state),
    }
  }
}

struct NormsProducerMerge<'a, D, CR>
where
  CR: CodecReader,
{
  merge_field_info: &'a Arc<FieldInfo>,
  merge_state: &'a MergeState<'a, D, CR>,
}

impl<D, CR> CloseableRef for NormsProducerMerge<'_, D, CR> where CR: CodecReader {}

impl<D, CR> NormsProducer for NormsProducerMerge<'_, D, CR>
where
  CR: CodecReader,
{
  type MergeInstance = Self;
  type NumericDocValues = NumericDocValuesMerge<
    crate::core::index::numeric_doc_values::NumericDocValuesEnum2<
      <CRNormsProducer<CR> as NormsProducer>::NumericDocValues,
      <<CRNormsProducer<CR> as NormsProducer>::MergeInstance as NormsProducer>::NumericDocValues,
    >,
    MergeStateDocMap<CR>,
  >;

  fn get_norms(&self, field_info: &Arc<FieldInfo>) -> Result<Self::NumericDocValues> {
    if !Arc::ptr_eq(field_info, self.merge_field_info) {
      return Err(LuceneError::illegal_argument("wrong fieldInfo"));
    }

    let producer_count = self
      .merge_state
      .norms_producers
      .iter()
      .filter(|producer| producer.is_some())
      .count();
    let mut subs = Vec::with_capacity(producer_count);
    debug_assert!(self.merge_state.doc_maps.len() == self.merge_state.doc_values_producers.len());
    for i in 0..self.merge_state.doc_values_producers.len() {
      let mut norms = None;
      let norms_producer_opt = &self.merge_state.norms_producers[i];
      if let Some(norms_producer) = norms_producer_opt {
        let reader_field_info =
          self.merge_state.field_infos[i].field_info_by_name(&self.merge_field_info.name)?;
        if let Some(reader_field_info) = &reader_field_info
          && reader_field_info.has_norms()
        {
          norms = Some(norms_producer.get_norms(reader_field_info)?);
        }
      }

      if let Some(norms) = norms {
        let doc_map = self.merge_state.doc_map(i);
        subs.push(Sub::new(NumericDocValuesSub::new(doc_map, norms)));
      }
    }

    let doc_id_merger = of(subs, self.merge_state.needs_index_sort)?;
    Ok(NumericDocValuesMerge {
      doc_id: -1,
      current: None,
      doc_id_merger,
    })
  }

  fn check_integrity(&self) -> Result<()> {
    Ok(())
  }
}

pub struct NumericDocValuesMerge<N, DM> {
  doc_id: i32,
  current: Option<usize>,
  doc_id_merger: DocIDMergerEnum<NumericDocValuesSub<N, DM>>,
}

impl<N, DM> DocValuesIterator for NumericDocValuesMerge<N, DM>
where
  N: NumericDocValues,
  DM: DocMap,
{
  fn advance_exact(&mut self, _target: i32) -> Result<bool> {
    Err(LuceneError::unsupported_operation(""))
  }
}

impl<N, DM> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for NumericDocValuesMerge<N, DM>
where
  N: NumericDocValues,
  DM: DocMap,
{
}
impl<N, DM> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for NumericDocValuesMerge<N, DM>
where
  N: NumericDocValues,
  DM: DocMap,
{
}

impl<N, DM> DocIdSetIterator for NumericDocValuesMerge<N, DM>
where
  N: NumericDocValues,
  DM: DocMap,
{
  fn doc_id(&self) -> i32 {
    self.doc_id
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.current = self.doc_id_merger.next()?;
    match self.current {
      Some(ref current) => {
        let v = self.doc_id_merger.get_subs()[*current].mapped_doc_id;
        self.doc_id = v;
        Ok(self.doc_id)
      },
      None => {
        self.doc_id = NO_MORE_DOCS;
        Ok(NO_MORE_DOCS)
      },
    }
  }

  fn advance(&mut self, _target: i32) -> Result<i32> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn cost(&self) -> Result<i64> {
    Ok(0)
  }
}

impl<N, DM> NumericDocValues for NumericDocValuesMerge<N, DM>
where
  N: NumericDocValues,
  DM: DocMap,
{
  fn long_value(&mut self) -> Result<i64> {
    match self.current {
      Some(ref current) => {
        let v = &mut self.doc_id_merger.get_subs_mut()[*current].sub;
        v.values.long_value()
      },
      None => Err(LuceneError::unreachable("")),
    }
  }
}
/// Tracks state of one numeric sub-reader that we are merging.
struct NumericDocValuesSub<N, DM> {
  values: N,
  doc_map: SharedDocMap<DM>,
}

impl<N, DM> NumericDocValuesSub<N, DM>
where
  N: DocIdSetIterator,
{
  fn new(doc_map: SharedDocMap<DM>, values: N) -> Self {
    debug_assert!(values.doc_id() == -1);
    NumericDocValuesSub { values, doc_map }
  }
}
impl<N, DM> SubBase for NumericDocValuesSub<N, DM>
where
  N: NumericDocValues,
  DM: DocMap,
{
  fn next_doc(&mut self) -> Result<i32> {
    self.values.next_doc()
  }

  type DocMap = DM;

  fn get_doc_map(&self) -> Result<&Self::DocMap> {
    Ok(&self.doc_map)
  }
}
