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
use crate::codec::bitvectors::hnsw_bit_vectors_format::HnswBitVectorsFormat;
use crate::codec::memory::direct_postings_format::DirectPostingsFormat;
use crate::core::codecs::Codec;
use crate::core::codecs::doc_values_consumer::DocValuesConsumer;
use crate::core::codecs::doc_values_format::DocValuesFormat;
use crate::core::codecs::fields_consumer::FieldsConsumer;
use crate::core::codecs::fields_producer::{FieldsProducer, FieldsProducerEnum2};
use crate::core::codecs::hnsw::hnsw_graph_provider::HnswGraphProvider;
use crate::core::codecs::knn_field_vectors_writer::VectorValueEnum;
use crate::core::codecs::knn_vectors_format::KnnVectorsFormat;
use crate::core::codecs::knn_vectors_formats::{KnnVectorsFormats, KnnVectorsFormatsReader};
use crate::core::codecs::knn_vectors_reader::KnnVectorsReader;
use crate::core::codecs::knn_vectors_writer::KnnVectorsWriter;
use crate::core::codecs::lucene99::lucene99_hnsw_scalar_quantized_vectors_format::Lucene99HnswScalarQuantizedVectorsFormat;
use crate::core::codecs::lucene99::lucene99_hnsw_vectors_format::Lucene99HnswVectorsFormat;
use crate::core::codecs::lucene99::lucene99_scalar_quantized_vectors_format::Lucene99ScalarQuantizedVectorsFormat;
use crate::core::codecs::perfield::per_field_doc_values_format::{
  PerFieldDocValuesFormat, PerFieldDocValuesFormatBase,
};
use crate::core::codecs::perfield::per_field_knn_vectors_format::{
  PerFieldKnnVectorsFormat, PerFieldKnnVectorsFormatBase,
};
use crate::core::codecs::perfield::per_field_postings_format::{
  PerFieldPostingsFormat, PerFieldPostingsFormatBase,
};
use crate::core::codecs::postings_format::PostingsFormat;
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::fields::Fields;
use crate::core::index::index_reader::Identity;
use crate::core::index::merge_state::{MergeState, MergeStateAccess};
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::sorter::DocMap;
use crate::core::index::terms::TermsEnum2;
use crate::core::search::knn_collector::KnnCollector;
use crate::core::store::directory::Directory;
use crate::core::store::{IndexInput, IndexOutput};
use crate::core::util::HasIdentity;
use crate::core::util::accountable::Accountable;
use crate::core::util::bits::Bits;
use crate::core::util::close::Closeable;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::quantization::scalar_quantizer::ScalarQuantizer;
use crate::test_framework::core::codecs::asserting_doc_values_format::{
  AssertingDocValuesFormat, AssertingDocValuesProducer,
};
use crate::test_framework::core::codecs::asserting_knn_vectors_format::AssertingKnnVectorsFormat;
use crate::test_framework::core::codecs::asserting_live_docs_format::AssertingLiveDocsFormat;
use crate::test_framework::core::codecs::asserting_norms_format::AssertingNormsFormat;
use crate::test_framework::core::codecs::asserting_points_format::AssertingPointsFormat;
use crate::test_framework::core::codecs::asserting_postings_format::AssertingPostingsFormat;
use crate::test_framework::core::codecs::asserting_stored_fields_format::AssertingStoredFieldsFormat;
use crate::test_framework::core::codecs::asserting_term_vectors_format::AssertingTermVectorsFormat;
use crate::test_framework::core::codecs::perfield::test_per_field_doc_values_format::{
  DocValuesMergeWithIndexedFieldsAssertingCodec, MergeCalledOnTwoFormatsAssertingCodec,
  MergeRecordingDocValueFormatWrapper, TwoFieldsTwoFormatsDocValuesAssertingCodec,
};
use crate::test_framework::core::codecs::perfield::test_per_field_knn_vectors_format::{
  KnnVectorsFormatMaxDims32, MaxDimensionsPerFieldFormatAssertingCodec,
  MergeUsesNewFormatAssertingCodec, TwoFieldsTwoFormatsAssertingCodec,
  WriteRecordingKnnVectorsFormat,
};
use crate::test_framework::core::codecs::perfield::test_per_field_postings_format::{
  MergeCalledOnTwoFormatsPostingsAssertingCodec, MergeRecordingPostingsFormatWrapper,
  MockAssertingCodec, SameCodecDifferentInstanceAssertingCodec,
};
use crate::test_framework::core::index::asserting_leaf_reader::AssertingTerms;
use crate::test_framework::core::index::test_add_indexes::CustomPerFieldAssertingCodec;
use crate::test_framework::core::util::test_util::{
  DefaultCodec, DefaultDocValuesFormat, DefaultPostingsFormat, TestUtil,
};
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::thread::ThreadId;

pub(crate) fn assert_thread(object: &str, creation_thread: ThreadId) {
  let current_thread = std::thread::current().id();
  assert!(
    creation_thread == current_thread,
    "{object} are only supposed to be consumed in the thread in which they have been acquired. \
     But was acquired in {creation_thread:?} and consumed in {current_thread:?}."
  );
}

pub enum AssertingCodecPostingsFormat {
  Default(Arc<DefaultPostingsFormat>),
  Asserting(Arc<AssertingPostingsFormat>),
  Direct(Arc<DirectPostingsFormat>),
  MergeRecording(Arc<MergeRecordingPostingsFormatWrapper>),
}

impl From<DefaultPostingsFormat> for AssertingCodecPostingsFormat {
  fn from(format: DefaultPostingsFormat) -> Self {
    Self::Default(Arc::new(format))
  }
}

impl From<AssertingPostingsFormat> for AssertingCodecPostingsFormat {
  fn from(format: AssertingPostingsFormat) -> Self {
    Self::Asserting(Arc::new(format))
  }
}

impl From<DirectPostingsFormat> for AssertingCodecPostingsFormat {
  fn from(format: DirectPostingsFormat) -> Self {
    Self::Direct(Arc::new(format))
  }
}

impl From<MergeRecordingPostingsFormatWrapper> for AssertingCodecPostingsFormat {
  fn from(format: MergeRecordingPostingsFormatWrapper) -> Self {
    Self::MergeRecording(Arc::new(format))
  }
}

impl Display for AssertingCodecPostingsFormat {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "PostingsFormat(name={})", self.get_name())
  }
}

impl HasIdentity for AssertingCodecPostingsFormat {
  fn identity(&self) -> &Identity {
    match self {
      Self::Default(format) => format.identity(),
      Self::Asserting(format) => format.identity(),
      Self::Direct(format) => format.identity(),
      Self::MergeRecording(format) => format.identity(),
    }
  }
}

pub enum AssertingCodecFieldsConsumer<O>
where
  O: IndexOutput,
{
  Default(<DefaultPostingsFormat as PostingsFormat>::FieldsConsumer<O>),
  Asserting(<AssertingPostingsFormat as PostingsFormat>::FieldsConsumer<O>),
  Direct(<DirectPostingsFormat as PostingsFormat>::FieldsConsumer<O>),
  MergeRecording(<MergeRecordingPostingsFormatWrapper as PostingsFormat>::FieldsConsumer<O>),
}

impl<O> Closeable for AssertingCodecFieldsConsumer<O>
where
  O: IndexOutput,
{
  fn close(&mut self) -> Result<()> {
    match self {
      Self::Default(consumer) => consumer.close(),
      Self::Asserting(consumer) => consumer.close(),
      Self::Direct(consumer) => consumer.close(),
      Self::MergeRecording(consumer) => consumer.close(),
    }
  }
}

impl<O> FieldsConsumer for AssertingCodecFieldsConsumer<O>
where
  O: IndexOutput,
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
    N: crate::core::codecs::norms_producer::NormsProducer,
  {
    match self {
      Self::Default(consumer) => consumer.write(state, segment_info, fields, norms),
      Self::Asserting(consumer) => consumer.write(state, segment_info, fields, norms),
      Self::Direct(consumer) => consumer.write(state, segment_info, fields, norms),
      Self::MergeRecording(consumer) => consumer.write(state, segment_info, fields, norms),
    }
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
    N: crate::core::codecs::norms_producer::NormsProducer,
    MS: MergeStateAccess,
  {
    match self {
      Self::Default(consumer) => consumer.merge(state, segment_info, merge_state, norms),
      Self::Asserting(consumer) => consumer.merge(state, segment_info, merge_state, norms),
      Self::Direct(consumer) => consumer.merge(state, segment_info, merge_state, norms),
      Self::MergeRecording(consumer) => {
        consumer.merge(state, segment_info, merge_state, norms)
      },
    }
  }
}

type DefaultAssertingCodecFieldsProducer<I> =
  <DefaultPostingsFormat as PostingsFormat>::FieldsProducer<I>;
type AssertingCodecFieldsProducerInner<I> =
  <DirectPostingsFormat as PostingsFormat>::FieldsProducer<I>;

#[derive(Clone, Copy)]
enum AssertingCodecFieldsProducerHook {
  Default,
  Asserting,
}

pub struct AssertingCodecFieldsProducer<I>
where
  I: IndexInput,
{
  in_: AssertingCodecFieldsProducerInner<I>,
  hook: AssertingCodecFieldsProducerHook,
}

impl<I> AssertingCodecFieldsProducer<I>
where
  I: IndexInput,
{
  pub(crate) fn default(in_: DefaultAssertingCodecFieldsProducer<I>) -> Self {
    Self {
      in_: FieldsProducerEnum2::A(in_),
      hook: AssertingCodecFieldsProducerHook::Default,
    }
  }

  fn asserting(in_: DefaultAssertingCodecFieldsProducer<I>) -> Self {
    Self {
      in_: FieldsProducerEnum2::A(in_),
      hook: AssertingCodecFieldsProducerHook::Asserting,
    }
  }

  fn direct(in_: AssertingCodecFieldsProducerInner<I>) -> Self {
    Self {
      in_,
      hook: AssertingCodecFieldsProducerHook::Default,
    }
  }
}

impl<I> CloseableRef for AssertingCodecFieldsProducer<I>
where
  I: IndexInput,
{
  fn close(&self) -> Result<()> {
    self.in_.close()?;
    match self.hook {
      AssertingCodecFieldsProducerHook::Default => Ok(()),
      AssertingCodecFieldsProducerHook::Asserting => self.in_.close(),
    }
  }
}

impl<I> Fields for AssertingCodecFieldsProducer<I>
where
  I: IndexInput,
{
  type FieldIter<'a>
    = <AssertingCodecFieldsProducerInner<I> as Fields>::FieldIter<'a>
  where
    Self: 'a;

  fn iterator(&self) -> Result<Self::FieldIter<'_>> {
    self.in_.iterator()
  }

  type Terms = AssertingTerms<<AssertingCodecFieldsProducerInner<I> as Fields>::Terms>;

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    match (&self.hook, &self.in_) {
      (AssertingCodecFieldsProducerHook::Asserting, FieldsProducerEnum2::A(in_)) => Ok(
        in_
          .terms(field)?
          .map(TermsEnum2::A)
          .map(AssertingTerms::new),
      ),
      (AssertingCodecFieldsProducerHook::Asserting, FieldsProducerEnum2::B(_)) => {
        unreachable!("asserting hook must wrap the default postings producer")
      },
      (AssertingCodecFieldsProducerHook::Default, _) => {
        Ok(self.in_.terms(field)?.map(AssertingTerms::new_default))
      },
    }
  }

  fn size(&self) -> Result<i32> {
    self.in_.size()
  }
}

impl<I> FieldsProducer for AssertingCodecFieldsProducer<I>
where
  I: IndexInput,
{
  fn check_integrity(&self) -> Result<()> {
    self.in_.check_integrity()
  }

  fn get_merge_instance(&self) -> Result<Option<Self>>
  where
    Self: Sized,
  {
    Ok(self.in_.get_merge_instance()?.map(|in_| Self {
      in_,
      hook: self.hook,
    }))
  }
}

impl PostingsFormat for AssertingCodecPostingsFormat {
  fn get_name(&self) -> &str {
    match self {
      Self::Default(format) => format.get_name(),
      Self::Asserting(format) => format.get_name(),
      Self::Direct(format) => format.get_name(),
      Self::MergeRecording(format) => format.get_name(),
    }
  }

  type FieldsConsumer<O: IndexOutput> = AssertingCodecFieldsConsumer<O>;

  fn fields_consumer<D1, D2>(
    &self,
    state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::FieldsConsumer<D1::IndexOutput>>
  where
    D1: Directory,
  {
    match self {
      Self::Default(format) => format
        .fields_consumer(state, segment_info)
        .map(AssertingCodecFieldsConsumer::Default),
      Self::Asserting(format) => format
        .fields_consumer(state, segment_info)
        .map(AssertingCodecFieldsConsumer::Asserting),
      Self::Direct(format) => format
        .fields_consumer(state, segment_info)
        .map(AssertingCodecFieldsConsumer::Direct),
      Self::MergeRecording(format) => format
        .fields_consumer(state, segment_info)
        .map(AssertingCodecFieldsConsumer::MergeRecording),
    }
  }

  type FieldsProducer<I: IndexInput> = AssertingCodecFieldsProducer<I>;

  fn fields_producer<D1, D2>(
    &self,
    state: &SegmentReadState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::FieldsProducer<D1::IndexInput>>
  where
    D1: Directory,
  {
    match self {
      Self::Default(format) => format
        .fields_producer(state, segment_info)
        .map(AssertingCodecFieldsProducer::default),
      Self::Asserting(format) => format
        .fields_producer(state, segment_info)
        .map(|producer| AssertingCodecFieldsProducer::asserting(producer.into_inner())),
      Self::Direct(format) => format
        .fields_producer(state, segment_info)
        .map(AssertingCodecFieldsProducer::direct),
      Self::MergeRecording(format) => format
        .fields_producer(state, segment_info)
        .map(AssertingCodecFieldsProducer::default),
    }
  }

  fn for_name(name: &str) -> Result<Arc<Self>> {
    match name {
      "Lucene101" => {
        DefaultPostingsFormat::for_name(name).map(|format| Arc::new(Self::Default(format)))
      },
      "Asserting" => {
        AssertingPostingsFormat::for_name(name).map(|format| Arc::new(Self::Asserting(format)))
      },
      "Direct" => DirectPostingsFormat::for_name(name).map(|format| Arc::new(Self::Direct(format))),
      _ => Err(LuceneError::illegal_argument(format!(
        "Could not load postings format named \"{name}\""
      ))),
    }
  }
}

pub enum AssertingCodecDocValuesFormat {
  Default(Arc<DefaultDocValuesFormat>),
  Asserting(Arc<AssertingDocValuesFormat>),
  MergeRecording(Arc<MergeRecordingDocValueFormatWrapper>),
}

impl From<DefaultDocValuesFormat> for AssertingCodecDocValuesFormat {
  fn from(format: DefaultDocValuesFormat) -> Self {
    Self::Default(Arc::new(format))
  }
}

impl From<AssertingDocValuesFormat> for AssertingCodecDocValuesFormat {
  fn from(format: AssertingDocValuesFormat) -> Self {
    Self::Asserting(Arc::new(format))
  }
}

impl From<MergeRecordingDocValueFormatWrapper> for AssertingCodecDocValuesFormat {
  fn from(format: MergeRecordingDocValueFormatWrapper) -> Self {
    Self::MergeRecording(Arc::new(format))
  }
}

impl Display for AssertingCodecDocValuesFormat {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Default(format) => Display::fmt(format, f),
      Self::Asserting(format) => Display::fmt(format, f),
      Self::MergeRecording(format) => Display::fmt(format, f),
    }
  }
}

impl HasIdentity for AssertingCodecDocValuesFormat {
  fn identity(&self) -> &Identity {
    match self {
      Self::Default(format) => format.identity(),
      Self::Asserting(format) => format.identity(),
      Self::MergeRecording(format) => format.identity(),
    }
  }
}

pub enum AssertingCodecDocValuesConsumer<O>
where
  O: IndexOutput,
{
  Default(<DefaultDocValuesFormat as DocValuesFormat>::DocValuesConsumer<O>),
  Asserting(<AssertingDocValuesFormat as DocValuesFormat>::DocValuesConsumer<O>),
  MergeRecording(<MergeRecordingDocValueFormatWrapper as DocValuesFormat>::DocValuesConsumer<O>),
}

impl<O> Closeable for AssertingCodecDocValuesConsumer<O>
where
  O: IndexOutput,
{
  fn close(&mut self) -> Result<()> {
    match self {
      Self::Default(consumer) => consumer.close(),
      Self::Asserting(consumer) => consumer.close(),
      Self::MergeRecording(consumer) => consumer.close(),
    }
  }
}

impl<O> DocValuesConsumer for AssertingCodecDocValuesConsumer<O>
where
  O: IndexOutput,
{
  type IndexOutput = O;

  fn add_numeric_field<D1, D2, D>(
    &mut self,
    write_state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
    field: &Arc<FieldInfo>,
    values_producer: &D,
  ) -> Result<()>
  where
    D1: Directory<IndexOutput = Self::IndexOutput>,
    D: crate::core::codecs::doc_values_producer::DocValuesProducer,
  {
    match self {
      Self::Default(consumer) => {
        consumer.add_numeric_field(write_state, segment_info, field, values_producer)
      },
      Self::Asserting(consumer) => {
        consumer.add_numeric_field(write_state, segment_info, field, values_producer)
      },
      Self::MergeRecording(consumer) => {
        consumer.add_numeric_field(write_state, segment_info, field, values_producer)
      },
    }
  }

  fn add_binary_field<D1, D2, D>(
    &mut self,
    write_state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
    field: &Arc<FieldInfo>,
    values_producer: &D,
  ) -> Result<()>
  where
    D1: Directory<IndexOutput = Self::IndexOutput>,
    D: crate::core::codecs::doc_values_producer::DocValuesProducer,
  {
    match self {
      Self::Default(consumer) => {
        consumer.add_binary_field(write_state, segment_info, field, values_producer)
      },
      Self::Asserting(consumer) => {
        consumer.add_binary_field(write_state, segment_info, field, values_producer)
      },
      Self::MergeRecording(consumer) => {
        consumer.add_binary_field(write_state, segment_info, field, values_producer)
      },
    }
  }

  fn add_sorted_field<D1, D2, D>(
    &mut self,
    write_state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
    field: &Arc<FieldInfo>,
    values_producer: &D,
  ) -> Result<()>
  where
    D1: Directory<IndexOutput = Self::IndexOutput>,
    D: crate::core::codecs::doc_values_producer::DocValuesProducer,
  {
    match self {
      Self::Default(consumer) => {
        consumer.add_sorted_field(write_state, segment_info, field, values_producer)
      },
      Self::Asserting(consumer) => {
        consumer.add_sorted_field(write_state, segment_info, field, values_producer)
      },
      Self::MergeRecording(consumer) => {
        consumer.add_sorted_field(write_state, segment_info, field, values_producer)
      },
    }
  }

  fn add_sorted_numeric_field<D1, D2, D>(
    &mut self,
    write_state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
    field: &Arc<FieldInfo>,
    values_producer: &D,
  ) -> Result<()>
  where
    D1: Directory<IndexOutput = Self::IndexOutput>,
    D: crate::core::codecs::doc_values_producer::DocValuesProducer,
  {
    match self {
      Self::Default(consumer) => {
        consumer.add_sorted_numeric_field(write_state, segment_info, field, values_producer)
      },
      Self::Asserting(consumer) => {
        consumer.add_sorted_numeric_field(write_state, segment_info, field, values_producer)
      },
      Self::MergeRecording(consumer) => {
        consumer.add_sorted_numeric_field(write_state, segment_info, field, values_producer)
      },
    }
  }

  fn add_sorted_set_field<D1, D2, D>(
    &mut self,
    write_state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
    field: &Arc<FieldInfo>,
    values_producer: &D,
  ) -> Result<()>
  where
    D1: Directory<IndexOutput = Self::IndexOutput>,
    D: crate::core::codecs::doc_values_producer::DocValuesProducer,
  {
    match self {
      Self::Default(consumer) => {
        consumer.add_sorted_set_field(write_state, segment_info, field, values_producer)
      },
      Self::Asserting(consumer) => {
        consumer.add_sorted_set_field(write_state, segment_info, field, values_producer)
      },
      Self::MergeRecording(consumer) => {
        consumer.add_sorted_set_field(write_state, segment_info, field, values_producer)
      },
    }
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
    match self {
      Self::Default(consumer) => consumer.merge(write_state, segment_info, merge_state),
      Self::Asserting(consumer) => consumer.merge(write_state, segment_info, merge_state),
      Self::MergeRecording(consumer) => consumer.merge(write_state, segment_info, merge_state),
    }
  }
}

pub type AssertingCodecDocValuesProducer<I> =
  AssertingDocValuesProducer<<DefaultDocValuesFormat as DocValuesFormat>::DocValuesProducer<I>>;

impl DocValuesFormat for AssertingCodecDocValuesFormat {
  fn get_name(&self) -> &str {
    match self {
      Self::Default(format) => format.get_name(),
      Self::Asserting(format) => format.get_name(),
      Self::MergeRecording(format) => format.get_name(),
    }
  }

  type DocValuesConsumer<O: IndexOutput> = AssertingCodecDocValuesConsumer<O>;

  fn fields_consumer<D1, D2>(
    &self,
    state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::DocValuesConsumer<D1::IndexOutput>>
  where
    D1: Directory,
  {
    match self {
      Self::Default(format) => format
        .fields_consumer(state, segment_info)
        .map(AssertingCodecDocValuesConsumer::Default),
      Self::Asserting(format) => format
        .fields_consumer(state, segment_info)
        .map(AssertingCodecDocValuesConsumer::Asserting),
      Self::MergeRecording(format) => format
        .fields_consumer(state, segment_info)
        .map(AssertingCodecDocValuesConsumer::MergeRecording),
    }
  }

  type DocValuesProducer<I: IndexInput> = AssertingCodecDocValuesProducer<I>;

  fn fields_producer<D1, D2>(
    &self,
    state: &SegmentReadState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::DocValuesProducer<D1::IndexInput>>
  where
    D1: Directory,
  {
    match self {
      Self::Default(format) => Ok(AssertingDocValuesProducer::new_default(
        format.fields_producer(state, segment_info)?,
      )),
      Self::Asserting(format) => format.fields_producer(state, segment_info),
      Self::MergeRecording(format) => Ok(AssertingDocValuesProducer::new_default(
        format.fields_producer(state, segment_info)?,
      )),
    }
  }

  fn for_name(name: &str) -> Result<Arc<Self>> {
    match name {
      "Lucene90" => {
        DefaultDocValuesFormat::for_name(name).map(|format| Arc::new(Self::Default(format)))
      },
      "Asserting" => {
        AssertingDocValuesFormat::for_name(name).map(|format| Arc::new(Self::Asserting(format)))
      },
      _ => Err(LuceneError::illegal_argument(format!(
        "Could not load doc values format named \"{name}\""
      ))),
    }
  }
}

#[derive(Clone)]
pub enum AssertingCodecKnnVectorsFormat {
  Asserting(Arc<AssertingKnnVectorsFormat>),
  Source(Arc<KnnVectorsFormats>),
  WriteRecording(Arc<WriteRecordingKnnVectorsFormat>),
  MaxDims32(Arc<KnnVectorsFormatMaxDims32>),
}

impl From<AssertingKnnVectorsFormat> for AssertingCodecKnnVectorsFormat {
  fn from(format: AssertingKnnVectorsFormat) -> Self {
    Self::Asserting(Arc::new(format))
  }
}

impl From<KnnVectorsFormats> for AssertingCodecKnnVectorsFormat {
  fn from(format: KnnVectorsFormats) -> Self {
    Self::Source(Arc::new(format))
  }
}

impl From<Lucene99HnswVectorsFormat> for AssertingCodecKnnVectorsFormat {
  fn from(format: Lucene99HnswVectorsFormat) -> Self {
    KnnVectorsFormats::from(format).into()
  }
}

impl From<Lucene99ScalarQuantizedVectorsFormat> for AssertingCodecKnnVectorsFormat {
  fn from(format: Lucene99ScalarQuantizedVectorsFormat) -> Self {
    KnnVectorsFormats::from(format).into()
  }
}

impl From<Lucene99HnswScalarQuantizedVectorsFormat> for AssertingCodecKnnVectorsFormat {
  fn from(format: Lucene99HnswScalarQuantizedVectorsFormat) -> Self {
    KnnVectorsFormats::from(format).into()
  }
}

impl From<HnswBitVectorsFormat> for AssertingCodecKnnVectorsFormat {
  fn from(format: HnswBitVectorsFormat) -> Self {
    KnnVectorsFormats::from(format).into()
  }
}

impl From<WriteRecordingKnnVectorsFormat> for AssertingCodecKnnVectorsFormat {
  fn from(format: WriteRecordingKnnVectorsFormat) -> Self {
    Self::WriteRecording(Arc::new(format))
  }
}

impl From<KnnVectorsFormatMaxDims32> for AssertingCodecKnnVectorsFormat {
  fn from(format: KnnVectorsFormatMaxDims32) -> Self {
    Self::MaxDims32(Arc::new(format))
  }
}

impl Display for AssertingCodecKnnVectorsFormat {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Asserting(format) => Display::fmt(format.as_ref(), f),
      Self::Source(format) => Display::fmt(format.as_ref(), f),
      Self::WriteRecording(format) => Display::fmt(format.as_ref(), f),
      Self::MaxDims32(format) => Display::fmt(format.as_ref(), f),
    }
  }
}

impl HasIdentity for AssertingCodecKnnVectorsFormat {
  fn identity(&self) -> &Identity {
    match self {
      Self::Asserting(format) => format.identity(),
      Self::Source(format) => format.identity(),
      Self::WriteRecording(format) => format.identity(),
      Self::MaxDims32(format) => format.identity(),
    }
  }
}

pub enum AssertingCodecKnnVectorsWriter<O: IndexOutput> {
  Asserting(<AssertingKnnVectorsFormat as KnnVectorsFormat>::KnnVectorsWriter<O>),
  Source(<KnnVectorsFormats as KnnVectorsFormat>::KnnVectorsWriter<O>),
  WriteRecording(<WriteRecordingKnnVectorsFormat as KnnVectorsFormat>::KnnVectorsWriter<O>),
}

impl<O: IndexOutput> Closeable for AssertingCodecKnnVectorsWriter<O> {
  fn close(&mut self) -> Result<()> {
    match self {
      Self::Asserting(inner) => inner.close(),
      Self::Source(inner) => inner.close(),
      Self::WriteRecording(inner) => inner.close(),
    }
  }
}

impl<O: IndexOutput> Accountable for AssertingCodecKnnVectorsWriter<O> {
  fn ram_bytes_used(&self) -> Result<i64> {
    match self {
      Self::Asserting(inner) => inner.ram_bytes_used(),
      Self::Source(inner) => inner.ram_bytes_used(),
      Self::WriteRecording(inner) => inner.ram_bytes_used(),
    }
  }
}

impl<O: IndexOutput> KnnVectorsWriter<O> for AssertingCodecKnnVectorsWriter<O> {
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
      Self::Asserting(inner) => inner.add_field(write_state, segment_info, field_info),
      Self::Source(inner) => inner.add_field(write_state, segment_info, field_info),
      Self::WriteRecording(inner) => inner.add_field(write_state, segment_info, field_info),
    }
  }

  fn flush<DM>(&mut self, max_doc: i32, sort_map: Option<&DM>) -> Result<()>
  where
    DM: DocMap,
  {
    match self {
      Self::Asserting(inner) => inner.flush(max_doc, sort_map),
      Self::Source(inner) => inner.flush(max_doc, sort_map),
      Self::WriteRecording(inner) => inner.flush(max_doc, sort_map),
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
      Self::Asserting(inner) => inner.merge_one_field(field_info, merge_state, segment_write_state),
      Self::Source(inner) => inner.merge_one_field(field_info, merge_state, segment_write_state),
      Self::WriteRecording(inner) => {
        inner.merge_one_field(field_info, merge_state, segment_write_state)
      },
    }
  }

  fn finish(&mut self) -> Result<()> {
    match self {
      Self::Asserting(inner) => inner.finish(),
      Self::Source(inner) => inner.finish(),
      Self::WriteRecording(inner) => inner.finish(),
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
      Self::Asserting(inner) => inner.merge(merge_state, segment_write_state),
      Self::Source(inner) => inner.merge(merge_state, segment_write_state),
      Self::WriteRecording(inner) => inner.merge(merge_state, segment_write_state),
    }
  }

  fn finish_merge<D, CR>(&self, merge_state: &MergeState<'_, D, CR>) -> Result<()>
  where
    CR: CodecReader,
  {
    match self {
      Self::Asserting(inner) => inner.finish_merge(merge_state),
      Self::Source(inner) => inner.finish_merge(merge_state),
      Self::WriteRecording(inner) => inner.finish_merge(merge_state),
    }
  }

  fn add_value(
    &mut self,
    doc_id: i32,
    vector_value: &VectorValueEnum,
    field_vectors_writers_idx: usize,
  ) -> Result<()> {
    match self {
      Self::Asserting(inner) => inner.add_value(doc_id, vector_value, field_vectors_writers_idx),
      Self::Source(inner) => inner.add_value(doc_id, vector_value, field_vectors_writers_idx),
      Self::WriteRecording(inner) => {
        inner.add_value(doc_id, vector_value, field_vectors_writers_idx)
      },
    }
  }
}

pub enum AssertingCodecKnnVectorsReader<I: IndexInput> {
  Asserting(<AssertingKnnVectorsFormat as KnnVectorsFormat>::KnnVectorsReader<I>),
  Source(<KnnVectorsFormats as KnnVectorsFormat>::KnnVectorsReader<I>),
}

impl<I: IndexInput> CloseableRef for AssertingCodecKnnVectorsReader<I> {
  fn close(&self) -> Result<()> {
    match self {
      Self::Asserting(reader) => reader.close(),
      Self::Source(reader) => reader.close(),
    }
  }
}

impl<I: IndexInput> HnswGraphProvider for AssertingCodecKnnVectorsReader<I> {
  type HnswGraph = <KnnVectorsFormatsReader<I> as HnswGraphProvider>::HnswGraph;

  fn is_hnsw_graph_provider(&self, field: &str) -> bool {
    match self {
      Self::Asserting(reader) => reader.is_hnsw_graph_provider(field),
      Self::Source(reader) => reader.is_hnsw_graph_provider(field),
    }
  }

  fn get_graph(&self, field: &str) -> Result<Self::HnswGraph> {
    match self {
      Self::Asserting(reader) => reader.get_graph(field),
      Self::Source(reader) => reader.get_graph(field),
    }
  }
}

impl<I: IndexInput> KnnVectorsReader for AssertingCodecKnnVectorsReader<I> {
  fn check_integrity(&self) -> Result<()> {
    match self {
      Self::Asserting(reader) => reader.check_integrity(),
      Self::Source(reader) => reader.check_integrity(),
    }
  }

  type FloatVectorValues = <KnnVectorsFormatsReader<I> as KnnVectorsReader>::FloatVectorValues;

  fn get_float_vector_values(&self, field: &str) -> Result<Self::FloatVectorValues> {
    match self {
      Self::Asserting(reader) => reader.get_float_vector_values(field),
      Self::Source(reader) => reader.get_float_vector_values(field),
    }
  }

  type ByteVectorValues = <KnnVectorsFormatsReader<I> as KnnVectorsReader>::ByteVectorValues;

  fn get_byte_vector_values(&self, field: &str) -> Result<Self::ByteVectorValues> {
    match self {
      Self::Asserting(reader) => reader.get_byte_vector_values(field),
      Self::Source(reader) => reader.get_byte_vector_values(field),
    }
  }

  fn get_quantization_state(&self, field: &str) -> Result<Option<ScalarQuantizer>> {
    match self {
      Self::Asserting(reader) => reader.get_quantization_state(field),
      Self::Source(reader) => reader.get_quantization_state(field),
    }
  }

  fn is_flat_vectors_reader(&self, field: &str) -> bool {
    match self {
      Self::Asserting(reader) => reader.is_flat_vectors_reader(field),
      Self::Source(reader) => reader.is_flat_vectors_reader(field),
    }
  }

  fn search_f32<B, K>(
    &self,
    field: &str,
    target: Vec<f32>,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
  {
    match self {
      Self::Asserting(reader) => reader.search_f32(field, target, knn_collector, accept_docs),
      Self::Source(reader) => reader.search_f32(field, target, knn_collector, accept_docs),
    }
  }

  fn search_u8<B, K>(
    &self,
    field: &str,
    target: Vec<u8>,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
  {
    match self {
      Self::Asserting(reader) => reader.search_u8(field, target, knn_collector, accept_docs),
      Self::Source(reader) => reader.search_u8(field, target, knn_collector, accept_docs),
    }
  }

  fn get_merge_instance(&self) -> Result<Option<Self>> {
    match self {
      Self::Asserting(reader) => reader
        .get_merge_instance()
        .map(|reader| reader.map(Self::Asserting)),
      Self::Source(reader) => reader
        .get_merge_instance()
        .map(|reader| reader.map(Self::Source)),
    }
  }

  fn finish_merge(&self) -> Result<()> {
    match self {
      Self::Asserting(reader) => reader.finish_merge(),
      Self::Source(reader) => reader.finish_merge(),
    }
  }
}

impl KnnVectorsFormat for AssertingCodecKnnVectorsFormat {
  fn get_name(&self) -> &str {
    match self {
      Self::Asserting(format) => format.get_name(),
      Self::Source(format) => format.get_name(),
      Self::WriteRecording(format) => format.get_name(),
      Self::MaxDims32(format) => format.get_name(),
    }
  }

  type KnnVectorsWriter<O: IndexOutput> = AssertingCodecKnnVectorsWriter<O>;

  fn fields_writer<D1, D2>(
    &self,
    state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::KnnVectorsWriter<D1::IndexOutput>>
  where
    D1: Directory,
  {
    match self {
      Self::Asserting(format) => format
        .fields_writer(state, segment_info)
        .map(AssertingCodecKnnVectorsWriter::Asserting),
      Self::Source(format) => format
        .fields_writer(state, segment_info)
        .map(AssertingCodecKnnVectorsWriter::Source),
      Self::WriteRecording(format) => format
        .fields_writer(state, segment_info)
        .map(AssertingCodecKnnVectorsWriter::WriteRecording),
      Self::MaxDims32(format) => format
        .fields_writer(state, segment_info)
        .map(AssertingCodecKnnVectorsWriter::Source),
    }
  }

  type KnnVectorsReader<I: IndexInput> = AssertingCodecKnnVectorsReader<I>;

  fn fields_reader<D1, D2>(
    &self,
    state: &SegmentReadState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::KnnVectorsReader<D1::IndexInput>>
  where
    D1: Directory,
  {
    match self {
      Self::Asserting(format) => format
        .fields_reader(state, segment_info)
        .map(AssertingCodecKnnVectorsReader::Asserting),
      Self::Source(format) => format
        .fields_reader(state, segment_info)
        .map(AssertingCodecKnnVectorsReader::Source),
      Self::WriteRecording(format) => format
        .fields_reader(state, segment_info)
        .map(AssertingCodecKnnVectorsReader::Source),
      Self::MaxDims32(format) => format
        .fields_reader(state, segment_info)
        .map(AssertingCodecKnnVectorsReader::Source),
    }
  }

  fn get_max_dimensions(&self, field_name: &str) -> Result<usize> {
    match self {
      Self::Asserting(format) => format.get_max_dimensions(field_name),
      Self::Source(format) => format.get_max_dimensions(field_name),
      Self::WriteRecording(format) => format.get_max_dimensions(field_name),
      Self::MaxDims32(format) => format.get_max_dimensions(field_name),
    }
  }

  fn for_name(name: &str) -> Result<Arc<Self>> {
    match name {
      "Asserting" => {
        AssertingKnnVectorsFormat::for_name(name).map(|format| Arc::new(Self::Asserting(format)))
      },
      _ => KnnVectorsFormats::for_name(name).map(|format| Arc::new(Self::Source(format))),
    }
  }
}

/// Static-dispatch access to the methods that Java subclasses override on
/// [`AssertingCodec`].
pub trait AssertingCodecBase {
  fn get_postings_format_for_field(&self, field: &str) -> Result<&AssertingCodecPostingsFormat>;

  fn get_doc_values_format_for_field(&self, field: &str) -> Result<&AssertingCodecDocValuesFormat>;

  fn get_knn_vectors_format_for_field(
    &self,
    field: &str,
  ) -> Result<&AssertingCodecKnnVectorsFormat>;
}

pub struct AssertingCodecDefaults {
  default_format: AssertingCodecPostingsFormat,
  default_dv_format: AssertingCodecDocValuesFormat,
  default_knn_vectors_format: AssertingCodecKnnVectorsFormat,
}

impl Default for AssertingCodecDefaults {
  fn default() -> Self {
    Self {
      default_format: AssertingPostingsFormat::new().into(),
      default_dv_format: AssertingDocValuesFormat::new().into(),
      default_knn_vectors_format: AssertingKnnVectorsFormat::new()
        .expect("default KNN vectors format parameters are valid")
        .into(),
    }
  }
}

impl AssertingCodecDefaults {
  /// Returns the postings format that should be used for writing new segments
  /// of `field`.
  ///
  /// The default implementation always returns `Asserting`.
  pub fn get_postings_format_for_field(
    &self,
    _field: &str,
  ) -> Result<&AssertingCodecPostingsFormat> {
    Ok(&self.default_format)
  }

  /// Returns the doc values format that should be used for writing new
  /// segments of `field`.
  ///
  /// The default implementation always returns `Asserting`.
  pub fn get_doc_values_format_for_field(
    &self,
    _field: &str,
  ) -> Result<&AssertingCodecDocValuesFormat> {
    Ok(&self.default_dv_format)
  }

  /// Returns the vectors format that should be used for writing new segments
  /// of `field`.
  ///
  /// The default implementation always returns `Asserting`.
  pub fn get_knn_vectors_format_for_field(
    &self,
    _field: &str,
  ) -> Result<&AssertingCodecKnnVectorsFormat> {
    Ok(&self.default_knn_vectors_format)
  }
}

impl AssertingCodecBase for AssertingCodecDefaults {
  fn get_postings_format_for_field(&self, field: &str) -> Result<&AssertingCodecPostingsFormat> {
    AssertingCodecDefaults::get_postings_format_for_field(self, field)
  }

  fn get_doc_values_format_for_field(&self, field: &str) -> Result<&AssertingCodecDocValuesFormat> {
    AssertingCodecDefaults::get_doc_values_format_for_field(self, field)
  }

  fn get_knn_vectors_format_for_field(
    &self,
    field: &str,
  ) -> Result<&AssertingCodecKnnVectorsFormat> {
    AssertingCodecDefaults::get_knn_vectors_format_for_field(self, field)
  }
}

pub(crate) struct AlwaysPostingsFormatAssertingCodec {
  defaults: AssertingCodecDefaults,
  format: AssertingCodecPostingsFormat,
}

impl AlwaysPostingsFormatAssertingCodec {
  fn new(format: AssertingCodecPostingsFormat) -> Self {
    Self {
      defaults: AssertingCodecDefaults::default(),
      format,
    }
  }
}

impl AssertingCodecBase for AlwaysPostingsFormatAssertingCodec {
  fn get_postings_format_for_field(&self, _field: &str) -> Result<&AssertingCodecPostingsFormat> {
    Ok(&self.format)
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

pub(crate) struct AlwaysDocValuesFormatAssertingCodec {
  defaults: AssertingCodecDefaults,
  format: AssertingCodecDocValuesFormat,
}

impl AlwaysDocValuesFormatAssertingCodec {
  fn new(format: AssertingCodecDocValuesFormat) -> Self {
    Self {
      defaults: AssertingCodecDefaults::default(),
      format,
    }
  }
}

impl AssertingCodecBase for AlwaysDocValuesFormatAssertingCodec {
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

pub(crate) struct AlwaysKnnVectorsFormatAssertingCodec {
  defaults: AssertingCodecDefaults,
  format: AssertingCodecKnnVectorsFormat,
}

impl AlwaysKnnVectorsFormatAssertingCodec {
  fn new(format: AssertingCodecKnnVectorsFormat) -> Self {
    Self {
      defaults: AssertingCodecDefaults::default(),
      format,
    }
  }
}

impl AssertingCodecBase for AlwaysKnnVectorsFormatAssertingCodec {
  fn get_postings_format_for_field(&self, field: &str) -> Result<&AssertingCodecPostingsFormat> {
    self.defaults.get_postings_format_for_field(field)
  }

  fn get_doc_values_format_for_field(&self, field: &str) -> Result<&AssertingCodecDocValuesFormat> {
    self.defaults.get_doc_values_format_for_field(field)
  }

  fn get_knn_vectors_format_for_field(
    &self,
    _field: &str,
  ) -> Result<&AssertingCodecKnnVectorsFormat> {
    Ok(&self.format)
  }
}

pub(crate) enum AssertingCodecHook {
  Default(AssertingCodecDefaults),
  AlwaysPostingsFormat(AlwaysPostingsFormatAssertingCodec),
  AlwaysDocValuesFormat(AlwaysDocValuesFormatAssertingCodec),
  AlwaysKnnVectorsFormat(AlwaysKnnVectorsFormatAssertingCodec),
  CustomPerField(CustomPerFieldAssertingCodec),
  TwoFieldsTwoFormats(TwoFieldsTwoFormatsAssertingCodec),
  MergeUsesNewFormat(MergeUsesNewFormatAssertingCodec),
  MaxDimensionsPerFieldFormat(MaxDimensionsPerFieldFormatAssertingCodec),
  TwoFieldsTwoFormatsDocValues(TwoFieldsTwoFormatsDocValuesAssertingCodec),
  MergeCalledOnTwoFormats(MergeCalledOnTwoFormatsAssertingCodec),
  DocValuesMergeWithIndexedFields(DocValuesMergeWithIndexedFieldsAssertingCodec),
  MockPostings(MockAssertingCodec),
  SameCodecDifferentInstance(SameCodecDifferentInstanceAssertingCodec),
  MergeCalledOnTwoFormatsPostings(MergeCalledOnTwoFormatsPostingsAssertingCodec),
}

impl Default for AssertingCodecHook {
  fn default() -> Self {
    Self::Default(AssertingCodecDefaults::default())
  }
}

impl AssertingCodecBase for AssertingCodecHook {
  fn get_postings_format_for_field(&self, field: &str) -> Result<&AssertingCodecPostingsFormat> {
    match self {
      Self::Default(defaults) => defaults.get_postings_format_for_field(field),
      Self::AlwaysPostingsFormat(hook) => hook.get_postings_format_for_field(field),
      Self::AlwaysDocValuesFormat(hook) => hook.get_postings_format_for_field(field),
      Self::AlwaysKnnVectorsFormat(hook) => hook.get_postings_format_for_field(field),
      Self::CustomPerField(hook) => hook.get_postings_format_for_field(field),
      Self::TwoFieldsTwoFormats(hook) => hook.get_postings_format_for_field(field),
      Self::MergeUsesNewFormat(hook) => hook.get_postings_format_for_field(field),
      Self::MaxDimensionsPerFieldFormat(hook) => hook.get_postings_format_for_field(field),
      Self::TwoFieldsTwoFormatsDocValues(hook) => hook.get_postings_format_for_field(field),
      Self::MergeCalledOnTwoFormats(hook) => hook.get_postings_format_for_field(field),
      Self::DocValuesMergeWithIndexedFields(hook) => hook.get_postings_format_for_field(field),
      Self::MockPostings(hook) => hook.get_postings_format_for_field(field),
      Self::SameCodecDifferentInstance(hook) => hook.get_postings_format_for_field(field),
      Self::MergeCalledOnTwoFormatsPostings(hook) => hook.get_postings_format_for_field(field),
    }
  }

  fn get_doc_values_format_for_field(&self, field: &str) -> Result<&AssertingCodecDocValuesFormat> {
    match self {
      Self::Default(defaults) => defaults.get_doc_values_format_for_field(field),
      Self::AlwaysPostingsFormat(hook) => hook.get_doc_values_format_for_field(field),
      Self::AlwaysDocValuesFormat(hook) => hook.get_doc_values_format_for_field(field),
      Self::AlwaysKnnVectorsFormat(hook) => hook.get_doc_values_format_for_field(field),
      Self::CustomPerField(hook) => hook.get_doc_values_format_for_field(field),
      Self::TwoFieldsTwoFormats(hook) => hook.get_doc_values_format_for_field(field),
      Self::MergeUsesNewFormat(hook) => hook.get_doc_values_format_for_field(field),
      Self::MaxDimensionsPerFieldFormat(hook) => hook.get_doc_values_format_for_field(field),
      Self::TwoFieldsTwoFormatsDocValues(hook) => hook.get_doc_values_format_for_field(field),
      Self::MergeCalledOnTwoFormats(hook) => hook.get_doc_values_format_for_field(field),
      Self::DocValuesMergeWithIndexedFields(hook) => hook.get_doc_values_format_for_field(field),
      Self::MockPostings(hook) => hook.get_doc_values_format_for_field(field),
      Self::SameCodecDifferentInstance(hook) => hook.get_doc_values_format_for_field(field),
      Self::MergeCalledOnTwoFormatsPostings(hook) => hook.get_doc_values_format_for_field(field),
    }
  }

  fn get_knn_vectors_format_for_field(
    &self,
    field: &str,
  ) -> Result<&AssertingCodecKnnVectorsFormat> {
    match self {
      Self::Default(defaults) => defaults.get_knn_vectors_format_for_field(field),
      Self::AlwaysPostingsFormat(hook) => hook.get_knn_vectors_format_for_field(field),
      Self::AlwaysDocValuesFormat(hook) => hook.get_knn_vectors_format_for_field(field),
      Self::AlwaysKnnVectorsFormat(hook) => hook.get_knn_vectors_format_for_field(field),
      Self::CustomPerField(hook) => hook.get_knn_vectors_format_for_field(field),
      Self::TwoFieldsTwoFormats(hook) => hook.get_knn_vectors_format_for_field(field),
      Self::MergeUsesNewFormat(hook) => hook.get_knn_vectors_format_for_field(field),
      Self::MaxDimensionsPerFieldFormat(hook) => hook.get_knn_vectors_format_for_field(field),
      Self::TwoFieldsTwoFormatsDocValues(hook) => hook.get_knn_vectors_format_for_field(field),
      Self::MergeCalledOnTwoFormats(hook) => hook.get_knn_vectors_format_for_field(field),
      Self::DocValuesMergeWithIndexedFields(hook) => hook.get_knn_vectors_format_for_field(field),
      Self::MockPostings(hook) => hook.get_knn_vectors_format_for_field(field),
      Self::SameCodecDifferentInstance(hook) => hook.get_knn_vectors_format_for_field(field),
      Self::MergeCalledOnTwoFormatsPostings(hook) => hook.get_knn_vectors_format_for_field(field),
    }
  }
}

pub struct AssertingCodecPostingsFormatBase {
  hook: Arc<AssertingCodecHook>,
}

impl PerFieldPostingsFormatBase for AssertingCodecPostingsFormatBase {
  type Format = AssertingCodecPostingsFormat;

  fn get_postings_format_for_field(&self, field: &str) -> Result<&Self::Format> {
    self.hook.get_postings_format_for_field(field)
  }
}

pub struct AssertingCodecDocValuesFormatBase {
  hook: Arc<AssertingCodecHook>,
}

impl PerFieldDocValuesFormatBase for AssertingCodecDocValuesFormatBase {
  type Format = AssertingCodecDocValuesFormat;

  fn get_doc_values_format_for_field(&self, field: &str) -> Result<&Self::Format> {
    self.hook.get_doc_values_format_for_field(field)
  }
}

pub struct AssertingCodecKnnVectorsFormatBase {
  hook: Arc<AssertingCodecHook>,
}

impl PerFieldKnnVectorsFormatBase for AssertingCodecKnnVectorsFormatBase {
  type Format = AssertingCodecKnnVectorsFormat;

  fn get_knn_vectors_format_for_field(&self, field: &str) -> Result<&Self::Format> {
    self.hook.get_knn_vectors_format_for_field(field)
  }
}

/// Acts like the default codec but with additional asserts.
pub struct AssertingCodec {
  delegate: DefaultCodec,
  postings: PerFieldPostingsFormat<AssertingCodecPostingsFormatBase>,
  doc_values: PerFieldDocValuesFormat<AssertingCodecDocValuesFormatBase>,
  knn_vectors_format: PerFieldKnnVectorsFormat<AssertingCodecKnnVectorsFormatBase>,
  hook: Arc<AssertingCodecHook>,
}

impl Default for AssertingCodec {
  fn default() -> Self {
    Self::new()
  }
}

impl AssertingCodec {
  pub fn new() -> Self {
    Self::with_hook(AssertingCodecHook::default())
  }

  pub(crate) fn with_postings_format(format: impl Into<AssertingCodecPostingsFormat>) -> Self {
    Self::with_hook(AssertingCodecHook::AlwaysPostingsFormat(
      AlwaysPostingsFormatAssertingCodec::new(format.into()),
    ))
  }

  pub(crate) fn with_doc_values_format(format: impl Into<AssertingCodecDocValuesFormat>) -> Self {
    Self::with_hook(AssertingCodecHook::AlwaysDocValuesFormat(
      AlwaysDocValuesFormatAssertingCodec::new(format.into()),
    ))
  }

  pub(crate) fn with_knn_vectors_format(format: impl Into<AssertingCodecKnnVectorsFormat>) -> Self {
    Self::with_hook(AssertingCodecHook::AlwaysKnnVectorsFormat(
      AlwaysKnnVectorsFormatAssertingCodec::new(format.into()),
    ))
  }

  pub(crate) fn with_hook(hook: AssertingCodecHook) -> Self {
    let hook = Arc::new(hook);
    Self {
      delegate: TestUtil::get_default_codec(),
      postings: PerFieldPostingsFormat::new(AssertingCodecPostingsFormatBase {
        hook: Arc::clone(&hook),
      }),
      doc_values: PerFieldDocValuesFormat::new(AssertingCodecDocValuesFormatBase {
        hook: Arc::clone(&hook),
      }),
      knn_vectors_format: PerFieldKnnVectorsFormat::new(AssertingCodecKnnVectorsFormatBase {
        hook: Arc::clone(&hook),
      }),
      hook,
    }
  }

  pub fn get_postings_format_for_field(
    &self,
    field: &str,
  ) -> Result<&AssertingCodecPostingsFormat> {
    self.hook.get_postings_format_for_field(field)
  }

  pub fn get_doc_values_format_for_field(
    &self,
    field: &str,
  ) -> Result<&AssertingCodecDocValuesFormat> {
    self.hook.get_doc_values_format_for_field(field)
  }

  pub fn get_knn_vectors_format_for_field(
    &self,
    field: &str,
  ) -> Result<&AssertingCodecKnnVectorsFormat> {
    self.hook.get_knn_vectors_format_for_field(field)
  }
}

impl Clone for AssertingCodec {
  fn clone(&self) -> Self {
    Self {
      delegate: self.delegate.clone(),
      postings: self.postings.clone(),
      doc_values: self.doc_values.clone(),
      knn_vectors_format: self.knn_vectors_format.clone(),
      hook: Arc::clone(&self.hook),
    }
  }
}

impl Codec for AssertingCodec {
  type PostingsFormat = PerFieldPostingsFormat<AssertingCodecPostingsFormatBase>;
  type DocValuesFormat = PerFieldDocValuesFormat<AssertingCodecDocValuesFormatBase>;
  type StoredFieldsFormat = AssertingStoredFieldsFormat;
  type TermVectorsFormat = AssertingTermVectorsFormat;
  type FieldInfosFormat = <DefaultCodec as Codec>::FieldInfosFormat;
  type SegmentInfoFormat = <DefaultCodec as Codec>::SegmentInfoFormat;
  type NormsFormat = AssertingNormsFormat;
  type LiveDocsFormat = AssertingLiveDocsFormat;
  type CompoundFormat = <DefaultCodec as Codec>::CompoundFormat;
  type PointsFormat = AssertingPointsFormat;
  type KnnVectorsFormat = PerFieldKnnVectorsFormat<AssertingCodecKnnVectorsFormatBase>;

  fn postings_format(&self) -> Self::PostingsFormat {
    self.postings.clone()
  }

  fn doc_values_format(&self) -> Self::DocValuesFormat {
    self.doc_values.clone()
  }

  fn stored_fields_format(&self) -> Self::StoredFieldsFormat {
    AssertingStoredFieldsFormat::new()
  }

  fn term_vectors_format(&self) -> Self::TermVectorsFormat {
    AssertingTermVectorsFormat::new()
  }

  fn field_infos_format(&self) -> Self::FieldInfosFormat {
    self.delegate.field_infos_format()
  }

  fn segment_info_format(&self) -> Self::SegmentInfoFormat {
    self.delegate.segment_info_format()
  }

  fn norms_format(&self) -> Self::NormsFormat {
    AssertingNormsFormat::new()
  }

  fn live_docs_format(&self) -> Self::LiveDocsFormat {
    AssertingLiveDocsFormat::new()
  }

  fn compound_format(&self) -> Self::CompoundFormat {
    self.delegate.compound_format()
  }

  fn points_format(&self) -> Self::PointsFormat {
    AssertingPointsFormat::new()
  }

  fn knn_vectors_format(&self) -> Result<Self::KnnVectorsFormat> {
    Ok(self.knn_vectors_format.clone())
  }

  fn get_name(&self) -> &str {
    "Asserting"
  }
}

impl Display for AssertingCodec {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "Asserting({})", self.delegate)
  }
}
