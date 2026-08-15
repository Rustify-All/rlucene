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
use crate::codec::memory::direct_postings_format::DirectPostingsFormat;
use crate::core::codecs::fields_consumer::FieldsConsumer;
use crate::core::codecs::norms_producer::NormsProducer;
use crate::core::codecs::postings_format::PostingsFormat;
use crate::core::index::fields::Fields;
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
use crate::test_framework::core::util::test_util::DefaultPostingsFormat;
use std::fmt::{Display, Formatter};
use std::sync::{Arc, Mutex};

#[allow(dead_code)] // for quick search
struct TestPerFieldPostingsFormat2;

pub(crate) struct MockAssertingCodec {
  defaults: AssertingCodecDefaults,
  direct: AssertingCodecPostingsFormat,
}

impl MockAssertingCodec {
  pub(crate) fn new() -> Self {
    Self {
      defaults: AssertingCodecDefaults::default(),
      direct: DirectPostingsFormat::new().into(),
    }
  }
}

impl AssertingCodecBase for MockAssertingCodec {
  fn get_postings_format_for_field(&self, field: &str) -> Result<&AssertingCodecPostingsFormat> {
    if field == "id" {
      Ok(&self.direct)
    } else {
      self.defaults.get_postings_format_for_field(field)
    }
  }

  fn get_doc_values_format_for_field(&self, field: &str) -> Result<&AssertingCodecDocValuesFormat> {
    self.defaults.get_doc_values_format_for_field(field)
  }

  fn get_knn_vectors_format_for_field(
    &self,
    field: &str,
  ) -> Result<&AssertingCodecKnnVectorsFormat> {
    self.defaults.get_knn_vectors_format_for_field(field)
  }
}

pub(crate) struct SameCodecDifferentInstanceAssertingCodec {
  defaults: AssertingCodecDefaults,
  id: AssertingCodecPostingsFormat,
  date: AssertingCodecPostingsFormat,
}

impl SameCodecDifferentInstanceAssertingCodec {
  pub(crate) fn new() -> Self {
    Self {
      defaults: AssertingCodecDefaults::default(),
      id: DirectPostingsFormat::new().into(),
      date: DirectPostingsFormat::new().into(),
    }
  }
}

impl AssertingCodecBase for SameCodecDifferentInstanceAssertingCodec {
  fn get_postings_format_for_field(&self, field: &str) -> Result<&AssertingCodecPostingsFormat> {
    match field {
      "id" => Ok(&self.id),
      "date" => Ok(&self.date),
      _ => self.defaults.get_postings_format_for_field(field),
    }
  }

  fn get_doc_values_format_for_field(&self, field: &str) -> Result<&AssertingCodecDocValuesFormat> {
    self.defaults.get_doc_values_format_for_field(field)
  }

  fn get_knn_vectors_format_for_field(
    &self,
    field: &str,
  ) -> Result<&AssertingCodecKnnVectorsFormat> {
    self.defaults.get_knn_vectors_format_for_field(field)
  }
}

#[derive(Default)]
struct MergeRecordingState {
  field_names: Vec<String>,
  nb_merge_calls: usize,
}

#[derive(Clone)]
pub struct MergeRecordingPostingsFormatWrapper {
  delegate: Arc<DefaultPostingsFormat>,
  identity: Identity,
  state: Arc<Mutex<MergeRecordingState>>,
}

impl MergeRecordingPostingsFormatWrapper {
  pub(crate) fn new(delegate: DefaultPostingsFormat) -> Self {
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

impl Display for MergeRecordingPostingsFormatWrapper {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "PostingsFormat(name={})", self.get_name())
  }
}

impl HasIdentity for MergeRecordingPostingsFormatWrapper {
  fn identity(&self) -> &Identity {
    &self.identity
  }
}

impl PostingsFormat for MergeRecordingPostingsFormatWrapper {
  fn get_name(&self) -> &str {
    self.delegate.get_name()
  }

  type FieldsConsumer<O: IndexOutput> =
    MergeRecordingFieldsConsumer<<DefaultPostingsFormat as PostingsFormat>::FieldsConsumer<O>>;

  fn fields_consumer<D1, D2>(
    &self,
    state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::FieldsConsumer<D1::IndexOutput>>
  where
    D1: Directory,
  {
    Ok(MergeRecordingFieldsConsumer {
      consumer: self.delegate.fields_consumer(state, segment_info)?,
      state: Arc::clone(&self.state),
    })
  }

  type FieldsProducer<I: IndexInput> = <DefaultPostingsFormat as PostingsFormat>::FieldsProducer<I>;

  fn fields_producer<D1, D2>(
    &self,
    state: &SegmentReadState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::FieldsProducer<D1::IndexInput>>
  where
    D1: Directory,
  {
    self.delegate.fields_producer(state, segment_info)
  }

  fn for_name(name: &str) -> Result<Arc<Self>> {
    Err(LuceneError::illegal_argument(format!(
      "Could not load postings format named \"{name}\""
    )))
  }
}

pub struct MergeRecordingFieldsConsumer<FC> {
  consumer: FC,
  state: Arc<Mutex<MergeRecordingState>>,
}

impl<FC> Closeable for MergeRecordingFieldsConsumer<FC>
where
  FC: FieldsConsumer,
{
  fn close(&mut self) -> Result<()> {
    self.consumer.close()
  }
}

impl<FC> FieldsConsumer for MergeRecordingFieldsConsumer<FC>
where
  FC: FieldsConsumer,
{
  fn write<D1, D2, F, N>(
    &mut self,
    state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
    fields: &mut F,
    norms: Option<&N>,
  ) -> Result<()>
  where
    D1: Directory,
    F: Fields,
    N: NormsProducer,
  {
    self.consumer.write(state, segment_info, fields, norms)
  }

  fn merge<D1, D2, N, MS>(
    &mut self,
    state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
    merge_state: &MS,
    norms: Option<&N>,
  ) -> Result<()>
  where
    D1: Directory,
    N: NormsProducer,
    MS: MergeStateAccess,
  {
    {
      let mut recording = self.state.lock().unwrap();
      recording.nb_merge_calls += 1;
      recording.field_names.extend(
        merge_state
          .merge_field_infos()
          .iter()
          .map(|field_info| field_info.name.clone()),
      );
    }
    self.consumer.merge(state, segment_info, merge_state, norms)
  }
}

pub(crate) struct MergeCalledOnTwoFormatsPostingsAssertingCodec {
  defaults: AssertingCodecDefaults,
  format1: AssertingCodecPostingsFormat,
  format2: AssertingCodecPostingsFormat,
}

impl MergeCalledOnTwoFormatsPostingsAssertingCodec {
  pub(crate) fn new(
    format1: AssertingCodecPostingsFormat,
    format2: AssertingCodecPostingsFormat,
  ) -> Self {
    Self {
      defaults: AssertingCodecDefaults::default(),
      format1,
      format2,
    }
  }
}

impl AssertingCodecBase for MergeCalledOnTwoFormatsPostingsAssertingCodec {
  fn get_postings_format_for_field(&self, field: &str) -> Result<&AssertingCodecPostingsFormat> {
    match field {
      "f1" | "f2" => Ok(&self.format1),
      "f3" | "f4" => Ok(&self.format2),
      _ => self.defaults.get_postings_format_for_field(field),
    }
  }

  fn get_doc_values_format_for_field(&self, field: &str) -> Result<&AssertingCodecDocValuesFormat> {
    self.defaults.get_doc_values_format_for_field(field)
  }

  fn get_knn_vectors_format_for_field(
    &self,
    field: &str,
  ) -> Result<&AssertingCodecKnnVectorsFormat> {
    self.defaults.get_knn_vectors_format_for_field(field)
  }
}
