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
use crate::core::codecs::norms_consumer::NormsConsumer;
use crate::core::codecs::norms_producer::NormsProducer;
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::docs_with_field_set::{DocsWithFieldSet, DocsWithFieldSetDISI};
use crate::core::index::field_info::FieldInfo;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::numeric_doc_values_writer::{SortingNumericDocValues, sort_doc_values};
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::sorter::DocMap;
use crate::core::search::doc_id_set::DocIdSet;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::util::accountable::Accountable;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::packed::PackedInts;
use crate::core::util::packed::packed_long_values::{
  Builder, PackedLongValues, PackedLongValuesIterator,
};
use crate::core::util::{Counter, SharedCounter};
use std::borrow::Borrow;
use std::sync::Arc;

/// Buffers up pending long per doc, then flushes when segment flushes.
pub(crate) struct NormValuesWriter {
  docs_with_field: DocsWithFieldSet,
  pending: Builder,
  iw_bytes_used: SharedCounter,
  bytes_used: i64,
  field_info: Arc<FieldInfo>,
  last_doc_id: i32,
}
impl NormValuesWriter {
  pub(crate) fn new(field_info: Arc<FieldInfo>, iw_bytes_used: SharedCounter) -> Result<Self> {
    let docs_with_field = DocsWithFieldSet::new();
    let pending = PackedLongValues::delta_packed_long_values_builder_default(PackedInts::COMPACT)?;
    let bytes_used = pending.ram_bytes_used()? + docs_with_field.ram_bytes_used()?;
    iw_bytes_used.add_and_get(bytes_used);
    Ok(Self {
      docs_with_field,
      pending,
      iw_bytes_used,
      bytes_used,
      field_info,
      last_doc_id: -1,
    })
  }
  pub(crate) fn add_value(&mut self, doc_id: i32, value: i64) -> Result<()> {
    if doc_id <= self.last_doc_id {
      return Err(LuceneError::illegal_argument(format!(
        "Norm for \"{}\" appears more than once in this document (only one value is allowed per field)",
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
    let new_bytes_used = self.pending.ram_bytes_used()? + self.docs_with_field.ram_bytes_used()?;
    let delta = new_bytes_used - self.bytes_used;
    if delta != 0 {
      self.iw_bytes_used.add_and_get(delta);
    }
    self.bytes_used = new_bytes_used;
    Ok(())
  }
  pub(crate) fn flush<D, DM, N>(
    &mut self,
    sort_map: Option<&DM>,
    norms_consumer: &mut N,
    segment_info: &SegmentInfo<D>,
  ) -> Result<()>
  where
    DM: DocMap,
    N: NormsConsumer,
  {
    let values = self.pending.build()?;
    self.docs_with_field.finish();
    if let Some(sort_map) = sort_map {
      let dense = sort_map.size() == self.docs_with_field.cardinality();
      let iter = self.docs_with_field.iterator()?;
      let mut buffer_norms = BufferedNorms::new(&values, iter)?;
      let sorted = sort_doc_values(segment_info.max_doc()?, sort_map, &mut buffer_norms, dense)?;
      let mut norms_producer = NormsProducerImpl {
        make_values: || Ok(SortingNumericDocValues::new(&sorted)),
      };
      norms_consumer.add_norms_field(&self.field_info, &mut norms_producer)?;
    } else {
      let mut norms_producer = NormsProducerImpl {
        make_values: || BufferedNorms::new(&values, self.docs_with_field.iterator()?),
      };
      norms_consumer.add_norms_field(&self.field_info, &mut norms_producer)?;
    }

    Ok(())
  }
}

struct NormsProducerImpl<F> {
  make_values: F,
}

impl<F> CloseableRef for NormsProducerImpl<F> {}

impl<F, V> NormsProducer for NormsProducerImpl<F>
where
  F: Fn() -> Result<V>,
  V: NumericDocValues,
{
  type MergeInstance = Self;
  type NumericDocValues = V;

  fn get_norms(&self, _field_info2: &Arc<FieldInfo>) -> Result<Self::NumericDocValues> {
    (self.make_values)()
  }

  fn check_integrity(&self) -> Result<()> {
    Ok(())
  }
}

/// iterates over the values we have in ram
pub(crate) struct BufferedNorms<V = PackedLongValues> {
  iter: PackedLongValuesIterator<V>,
  doc_with_field: DocsWithFieldSetDISI,
  value: i64,
}
impl<V: Borrow<PackedLongValues>> BufferedNorms<V> {
  pub(crate) fn new(values: V, doc_with_field: DocsWithFieldSetDISI) -> Result<Self> {
    Ok(Self {
      iter: PackedLongValuesIterator::new(values)?,
      doc_with_field,
      value: 0,
    })
  }
}

impl<V: Borrow<PackedLongValues>> DocValuesIterator for BufferedNorms<V> {
  fn advance_exact(&mut self, _target: i32) -> Result<bool> {
    Err(LuceneError::unsupported_operation(""))
  }
}

impl<V: Borrow<PackedLongValues>>
  crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions for BufferedNorms<V>
{
}
impl<V: Borrow<PackedLongValues>> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for BufferedNorms<V>
{
}

impl<V: Borrow<PackedLongValues>> DocIdSetIterator for BufferedNorms<V> {
  fn doc_id(&self) -> i32 {
    self.doc_with_field.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    let doc_id = self.doc_with_field.next_doc()?;
    if doc_id != NO_MORE_DOCS {
      self.value = self.iter.next_value()?;
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

impl<V: Borrow<PackedLongValues>> NumericDocValues for BufferedNorms<V> {
  fn long_value(&mut self) -> Result<i64> {
    Ok(self.value)
  }
}
