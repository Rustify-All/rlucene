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
use crate::core::codecs::doc_values_format::DocValuesFormat;
use crate::core::codecs::doc_values_producer::DocValuesProducer;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::index_reader::Identity;
use crate::core::index::merge_state::MergeStateAccess;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::store::directory::Directory;
use crate::core::store::{IndexInput, IndexOutput};
use crate::core::util::HasIdentity;
use crate::core::util::close::Closeable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::test_framework::core::codecs::asserting_codec::{
  AssertingCodecBase, AssertingCodecDefaults, AssertingCodecDocValuesFormat,
  AssertingCodecKnnVectorsFormat, AssertingCodecPostingsFormat,
};
use crate::test_framework::core::util::test_util::DefaultDocValuesFormat;
use std::fmt::{Display, Formatter};
use std::sync::{Arc, Mutex};

#[allow(dead_code)] // for quick search
struct TestPerFieldDocValuesFormat;

#[derive(Default)]
struct MergeRecordingState {
  field_names: Vec<String>,
  nb_merge_calls: usize,
}

#[derive(Clone)]
pub struct MergeRecordingDocValueFormatWrapper {
  delegate: Arc<DefaultDocValuesFormat>,
  identity: Identity,
  state: Arc<Mutex<MergeRecordingState>>,
}

impl MergeRecordingDocValueFormatWrapper {
  pub(crate) fn new(delegate: DefaultDocValuesFormat) -> Self {
    Self {
      delegate: Arc::new(delegate),
      identity: Identity::new(),
      state: Arc::new(Mutex::new(MergeRecordingState::default())),
    }
  }

  pub(crate) fn nb_merge_calls(&self) -> usize {
    self.state.lock().unwrap().nb_merge_calls
  }

  pub(crate) fn field_names(&self) -> Vec<String> {
    self.state.lock().unwrap().field_names.clone()
  }
}

impl Display for MergeRecordingDocValueFormatWrapper {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    Display::fmt(self.delegate.as_ref(), f)
  }
}

impl HasIdentity for MergeRecordingDocValueFormatWrapper {
  fn identity(&self) -> &Identity {
    &self.identity
  }
}

impl DocValuesFormat for MergeRecordingDocValueFormatWrapper {
  fn get_name(&self) -> &str {
    self.delegate.get_name()
  }

  type DocValuesConsumer<O: IndexOutput> = MergeRecordingDocValuesConsumer<
    <DefaultDocValuesFormat as DocValuesFormat>::DocValuesConsumer<O>,
  >;

  fn fields_consumer<D1, D2>(
    &self,
    state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::DocValuesConsumer<D1::IndexOutput>>
  where
    D1: Directory,
  {
    Ok(MergeRecordingDocValuesConsumer {
      consumer: self.delegate.fields_consumer(state, segment_info)?,
      state: Arc::clone(&self.state),
    })
  }

  type DocValuesProducer<I: IndexInput> =
    <DefaultDocValuesFormat as DocValuesFormat>::DocValuesProducer<I>;

  fn fields_producer<D1, D2>(
    &self,
    state: &SegmentReadState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::DocValuesProducer<D1::IndexInput>>
  where
    D1: Directory,
  {
    self.delegate.fields_producer(state, segment_info)
  }

  fn for_name(name: &str) -> Result<Arc<Self>> {
    Err(LuceneError::illegal_argument(format!(
      "Could not load doc values format named \"{name}\""
    )))
  }
}

pub struct MergeRecordingDocValuesConsumer<DVC> {
  consumer: DVC,
  state: Arc<Mutex<MergeRecordingState>>,
}

impl<DVC> Closeable for MergeRecordingDocValuesConsumer<DVC>
where
  DVC: DocValuesConsumer,
{
  fn close(&mut self) -> Result<()> {
    self.consumer.close()
  }
}

impl<DVC> DocValuesConsumer for MergeRecordingDocValuesConsumer<DVC>
where
  DVC: DocValuesConsumer,
{
  type IndexOutput = DVC::IndexOutput;

  fn add_numeric_field<D1, D2, D>(
    &mut self,
    write_state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
    field: &Arc<FieldInfo>,
    values: &D,
  ) -> Result<()>
  where
    D1: Directory<IndexOutput = Self::IndexOutput>,
    D: DocValuesProducer,
  {
    self
      .consumer
      .add_numeric_field(write_state, segment_info, field, values)
  }

  fn add_binary_field<D1, D2, D>(
    &mut self,
    write_state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
    field: &Arc<FieldInfo>,
    values: &D,
  ) -> Result<()>
  where
    D1: Directory<IndexOutput = Self::IndexOutput>,
    D: DocValuesProducer,
  {
    self
      .consumer
      .add_binary_field(write_state, segment_info, field, values)
  }

  fn add_sorted_field<D1, D2, D>(
    &mut self,
    write_state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
    field: &Arc<FieldInfo>,
    values: &D,
  ) -> Result<()>
  where
    D1: Directory<IndexOutput = Self::IndexOutput>,
    D: DocValuesProducer,
  {
    self
      .consumer
      .add_sorted_field(write_state, segment_info, field, values)
  }

  fn add_sorted_numeric_field<D1, D2, D>(
    &mut self,
    write_state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
    field: &Arc<FieldInfo>,
    values: &D,
  ) -> Result<()>
  where
    D1: Directory<IndexOutput = Self::IndexOutput>,
    D: DocValuesProducer,
  {
    self
      .consumer
      .add_sorted_numeric_field(write_state, segment_info, field, values)
  }

  fn add_sorted_set_field<D1, D2, D>(
    &mut self,
    write_state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
    field: &Arc<FieldInfo>,
    values: &D,
  ) -> Result<()>
  where
    D1: Directory<IndexOutput = Self::IndexOutput>,
    D: DocValuesProducer,
  {
    self
      .consumer
      .add_sorted_set_field(write_state, segment_info, field, values)
  }

  fn merge<D1, D2, MS>(
    &mut self,
    write_state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
    merge_state: &MS,
  ) -> Result<()>
  where
    D1: Directory<IndexOutput = Self::IndexOutput>,
    MS: MergeStateAccess,
  {
    {
      let mut state = self.state.lock().unwrap();
      state.nb_merge_calls += 1;
      state.field_names.extend(
        merge_state
          .merge_field_infos()
          .iter()
          .map(|field_info| field_info.name.clone()),
      );
    }
    self.consumer.merge(write_state, segment_info, merge_state)
  }
}

pub(crate) struct TwoFieldsTwoFormatsDocValuesAssertingCodec {
  defaults: AssertingCodecDefaults,
  fast: AssertingCodecDocValuesFormat,
  slow: AssertingCodecDocValuesFormat,
}

impl TwoFieldsTwoFormatsDocValuesAssertingCodec {
  pub(crate) fn new(
    fast: AssertingCodecDocValuesFormat,
    slow: AssertingCodecDocValuesFormat,
  ) -> Self {
    Self {
      defaults: AssertingCodecDefaults::default(),
      fast,
      slow,
    }
  }
}

impl AssertingCodecBase for TwoFieldsTwoFormatsDocValuesAssertingCodec {
  fn get_postings_format_for_field(&self, field: &str) -> Result<&AssertingCodecPostingsFormat> {
    self.defaults.get_postings_format_for_field(field)
  }

  fn get_doc_values_format_for_field(&self, field: &str) -> Result<&AssertingCodecDocValuesFormat> {
    if field == "dv1" {
      Ok(&self.fast)
    } else {
      Ok(&self.slow)
    }
  }

  fn get_knn_vectors_format_for_field(
    &self,
    field: &str,
  ) -> Result<&AssertingCodecKnnVectorsFormat> {
    self.defaults.get_knn_vectors_format_for_field(field)
  }
}

pub(crate) struct MergeCalledOnTwoFormatsAssertingCodec {
  defaults: AssertingCodecDefaults,
  format1: AssertingCodecDocValuesFormat,
  format2: AssertingCodecDocValuesFormat,
}

impl MergeCalledOnTwoFormatsAssertingCodec {
  pub(crate) fn new(
    format1: AssertingCodecDocValuesFormat,
    format2: AssertingCodecDocValuesFormat,
  ) -> Self {
    Self {
      defaults: AssertingCodecDefaults::default(),
      format1,
      format2,
    }
  }
}

impl AssertingCodecBase for MergeCalledOnTwoFormatsAssertingCodec {
  fn get_postings_format_for_field(&self, field: &str) -> Result<&AssertingCodecPostingsFormat> {
    self.defaults.get_postings_format_for_field(field)
  }

  fn get_doc_values_format_for_field(&self, field: &str) -> Result<&AssertingCodecDocValuesFormat> {
    match field {
      "dv1" | "dv2" => Ok(&self.format1),
      "dv3" => Ok(&self.format2),
      _ => self.defaults.get_doc_values_format_for_field(field),
    }
  }

  fn get_knn_vectors_format_for_field(
    &self,
    field: &str,
  ) -> Result<&AssertingCodecKnnVectorsFormat> {
    self.defaults.get_knn_vectors_format_for_field(field)
  }
}

pub(crate) struct DocValuesMergeWithIndexedFieldsAssertingCodec {
  defaults: AssertingCodecDefaults,
  format: AssertingCodecDocValuesFormat,
}

impl DocValuesMergeWithIndexedFieldsAssertingCodec {
  pub(crate) fn new(format: AssertingCodecDocValuesFormat) -> Self {
    Self {
      defaults: AssertingCodecDefaults::default(),
      format,
    }
  }
}

impl AssertingCodecBase for DocValuesMergeWithIndexedFieldsAssertingCodec {
  fn get_postings_format_for_field(&self, field: &str) -> Result<&AssertingCodecPostingsFormat> {
    self.defaults.get_postings_format_for_field(field)
  }

  fn get_doc_values_format_for_field(
    &self,
    _field: &str,
  ) -> Result<&AssertingCodecDocValuesFormat> {
    Ok(&self.format)
  }

  fn get_knn_vectors_format_for_field(
    &self,
    field: &str,
  ) -> Result<&AssertingCodecKnnVectorsFormat> {
    self.defaults.get_knn_vectors_format_for_field(field)
  }
}
