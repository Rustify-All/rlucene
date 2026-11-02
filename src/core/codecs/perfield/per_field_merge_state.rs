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

use crate::core::codecs::fields_producer::FieldsProducer;
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::field_infos::{FieldInfos, FieldInfosHook, FilterFieldInfosHook};
use crate::core::index::fields::Fields;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::merge_state::{MergeStateAccess, MergeStateMeta};
use crate::core::search::task_executor::TaskExecutor;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::iterator::IteratorExt;
use std::borrow::Borrow;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

/// Utility creating a merge-state view restricted to a set of fields.
pub(crate) struct PerFieldMergeState<'a, MS, N>
where
  MS: MergeStateAccess,
  N: Borrow<String>,
{
  in_: &'a MS,
  merge_field_infos: Arc<FieldInfos>,
  field_infos: Rc<Vec<Arc<FieldInfos>>>,
  fields_producers: Vec<Option<FilterFieldsProducer<'a, MS::FieldsProducer, N>>>,
}

impl<'a, MS, N> PerFieldMergeState<'a, MS, N>
where
  MS: MergeStateAccess,
  N: Borrow<String>,
{
  /// Creates a new merge-state view from `in_` that only exposes `fields`.
  pub(crate) fn restrict_fields(in_: &'a MS, fields: &'a [N]) -> Result<Self> {
    let filtered_names = Arc::new(
      fields
        .iter()
        .map(|field| field.borrow().clone())
        .collect::<HashSet<_>>(),
    );
    let mut field_infos = Vec::with_capacity(in_.field_infos().len());
    for info in in_.field_infos() {
      field_infos.push(Self::new_filter(info, &filtered_names)?);
    }
    let mut fields_producers = Vec::with_capacity(in_.fields_producers().len());
    for producer in in_.fields_producers() {
      fields_producers.push(match producer {
        Some(producer) => Some(FilterFieldsProducer::new(producer, fields)),
        None => None,
      });
    }

    Ok(Self {
      in_,
      merge_field_infos: Self::new_filter(in_.merge_field_infos(), &filtered_names)?,
      field_infos: Rc::new(field_infos),
      fields_producers,
    })
  }

  fn new_filter(
    src: &FieldInfos,
    filtered_names: &Arc<HashSet<String>>,
  ) -> Result<Arc<FieldInfos>> {
    // Copy all the input FieldInfo objects since the field numbering must be kept consistent
    let mut field_infos = FieldInfos::new(src.iter().cloned().collect())?;

    let mut has_vectors = false;
    let mut has_postings = false;
    let mut has_prox = false;
    let mut has_payloads = false;
    let mut has_offsets = false;
    let mut has_freq = false;
    let mut has_norms = false;
    let mut has_doc_values = false;
    let mut has_point_values = false;

    let mut filtered = Vec::with_capacity(filtered_names.len());
    for fi in src {
      if filtered_names.contains(&fi.name) {
        filtered.push(fi.clone());
        has_vectors |= fi.has_term_vectors();
        has_postings |= fi.get_index_options() != &IndexOptions::None;
        has_prox |= fi.get_index_options() >= &IndexOptions::DocsAndFreqsAndPositions;
        has_freq |= fi.get_index_options() != &IndexOptions::Docs;
        has_offsets |= fi.get_index_options() >= &IndexOptions::DocsAndFreqsAndPositionsAndOffsets;
        has_norms |= fi.has_norms();
        has_doc_values |= fi.get_doc_values_type() != &DocValuesType::None;
        has_payloads |= fi.has_payloads();
        has_point_values |= fi.get_point_dimension_count() != 0;
      }
    }

    field_infos.hook = FieldInfosHook::Filter(FilterFieldInfosHook {
      filtered_names: filtered_names.clone(),
      filtered: filtered.into_boxed_slice(),
      filtered_has_vectors: has_vectors,
      filtered_has_postings: has_postings,
      filtered_has_prox: has_prox,
      filtered_has_payloads: has_payloads,
      filtered_has_offsets: has_offsets,
      filtered_has_freq: has_freq,
      filtered_has_norms: has_norms,
      filtered_has_doc_values: has_doc_values,
      filtered_has_point_values: has_point_values,
    });
    Ok(Arc::new(field_infos))
  }
}

impl<'a, MS, N> MergeStateAccess for PerFieldMergeState<'a, MS, N>
where
  MS: MergeStateAccess,
  N: Borrow<String>,
{
  type FieldsProducer = FilterFieldsProducer<'a, MS::FieldsProducer, N>;
  type DocValuesProducer = MS::DocValuesProducer;
  type LiveDocs = MS::LiveDocs;
  type DocMap = MS::DocMap;

  fn fields_producers(&self) -> &[Option<Self::FieldsProducer>] {
    &self.fields_producers
  }

  fn doc_values_producers(&self) -> &[Option<Self::DocValuesProducer>] {
    self.in_.doc_values_producers()
  }

  fn doc_maps(&self) -> &std::rc::Rc<Vec<Self::DocMap>> {
    self.in_.doc_maps()
  }

  fn merge_field_infos(&self) -> &Arc<FieldInfos> {
    &self.merge_field_infos
  }

  fn field_infos(&self) -> &[Arc<FieldInfos>] {
    &self.field_infos
  }

  fn live_docs(&self) -> &[Option<Self::LiveDocs>] {
    self.in_.live_docs()
  }

  fn needs_index_sort(&self) -> bool {
    self.in_.needs_index_sort()
  }

  fn max_docs(&self) -> &[i32] {
    self.in_.max_docs()
  }

  fn intra_merge_task_executor(&self) -> &Arc<TaskExecutor> {
    self.in_.intra_merge_task_executor()
  }

  fn get_meta(&self) -> MergeStateMeta<Self::DocMap> {
    let mut meta = self.in_.get_meta();
    meta.fields_producers_len = self.fields_producers.len();
    meta.merge_field_infos = self.merge_field_infos.clone();
    meta.field_infos = self.field_infos.clone();
    meta
  }
}

pub(crate) struct FilterFieldsProducer<'a, P, N> {
  in_: &'a P,
  filtered: &'a [N],
}

impl<'a, P, N> FilterFieldsProducer<'a, P, N> {
  fn new(in_: &'a P, filtered: &'a [N]) -> Self {
    Self { in_, filtered }
  }
}

pub(crate) struct FilterFieldsIterator<'a, N> {
  fields: std::slice::Iter<'a, N>,
}

impl<'a, N> FilterFieldsIterator<'a, N> {
  pub(super) fn new(fields: &'a [N]) -> Self {
    Self {
      fields: fields.iter(),
    }
  }
}

impl<'a, N> IteratorExt for FilterFieldsIterator<'a, N>
where
  N: Borrow<String>,
{
  type Item = &'a String;

  fn next(&mut self) -> Result<Option<Self::Item>> {
    Ok(self.fields.next().map(Borrow::borrow))
  }

  fn has_next(&self) -> Result<bool> {
    Ok(!self.fields.as_slice().is_empty())
  }
}

impl<P, N> Fields for FilterFieldsProducer<'_, P, N>
where
  P: FieldsProducer,
  N: Borrow<String>,
{
  type FieldIter<'a>
    = FilterFieldsIterator<'a, N>
  where
    Self: 'a;

  fn iterator(&self) -> Result<Self::FieldIter<'_>> {
    Ok(FilterFieldsIterator::new(self.filtered))
  }

  type Terms = P::Terms;

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    if !self
      .filtered
      .iter()
      .any(|filtered| filtered.borrow() == field)
    {
      let available_fields = self
        .filtered
        .iter()
        .map(|field| field.borrow().as_str())
        .collect::<Vec<_>>()
        .join(", ");
      return Err(LuceneError::illegal_argument(format!(
        "The field named '{field}' is not accessible in the current merge context, available ones are: [{available_fields}]"
      )));
    }
    self.in_.terms(field)
  }

  fn size(&self) -> Result<i32> {
    Ok(self.filtered.len() as i32)
  }
}

impl<P, N> CloseableRef for FilterFieldsProducer<'_, P, N>
where
  P: FieldsProducer,
  N: Borrow<String>,
{
  fn close(&self) -> Result<()> {
    self.in_.close()
  }
}

impl<P, N> FieldsProducer for FilterFieldsProducer<'_, P, N>
where
  P: FieldsProducer,
  N: Borrow<String>,
{
  fn check_integrity(&self) -> Result<()> {
    self.in_.check_integrity()
  }
}
