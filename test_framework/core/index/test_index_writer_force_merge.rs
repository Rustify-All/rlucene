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
use crate::core::codecs::Codec;
use crate::core::codecs::doc_values_consumer::DocValuesConsumer;
use crate::core::codecs::doc_values_format::DocValuesFormat;
use crate::core::codecs::doc_values_producer::DocValuesProducer;
use crate::core::codecs::fields_consumer::FieldsConsumer;
use crate::core::codecs::norms_producer::NormsProducer;
use crate::core::codecs::perfield::per_field_doc_values_format::{
  PerFieldDocValuesFormat, PerFieldDocValuesFormatBase,
};
use crate::core::codecs::perfield::per_field_postings_format::{
  PerFieldPostingsFormat, PerFieldPostingsFormatBase,
};
use crate::core::codecs::postings_format::PostingsFormat;
use crate::core::index::field_info::FieldInfo;
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
use crate::test_framework::core::util::test_util::{
  DefaultCodec, DefaultDocValuesFormat, DefaultPostingsFormat, TestUtil,
};
use std::fmt::{Display, Formatter};
use std::sync::{Arc, Barrier};

#[allow(dead_code)] // for quick search
struct TestIndexWriterForceMerge;

pub type MergePerFieldPostingsFormat = PerFieldPostingsFormat<MergePerFieldPostingsFormatBase>;
pub type MergePerFieldDocValuesFormat = PerFieldDocValuesFormat<MergePerFieldDocValuesFormatBase>;

pub struct MergePerFieldPostingsFormatBase {
  postings_formats: Vec<BlockingOnMergePostingsFormat>,
}

impl PerFieldPostingsFormatBase for MergePerFieldPostingsFormatBase {
  type Format = BlockingOnMergePostingsFormat;

  fn get_postings_format_for_field(&self, field: &str) -> Result<&Self::Format> {
    let index = field
      .strip_prefix('f')
      .and_then(|index| index.parse::<usize>().ok())
      .filter(|index| *index < self.postings_formats.len())
      .ok_or_else(|| {
        LuceneError::illegal_argument(format!("unexpected postings field: {field}"))
      })?;
    Ok(&self.postings_formats[index])
  }
}

pub struct MergePerFieldDocValuesFormatBase {
  doc_values_formats: Vec<BlockingOnMergeDocValuesFormat>,
}

impl PerFieldDocValuesFormatBase for MergePerFieldDocValuesFormatBase {
  type Format = BlockingOnMergeDocValuesFormat;

  fn get_doc_values_format_for_field(&self, field: &str) -> Result<&Self::Format> {
    let index = field
      .strip_prefix('f')
      .and_then(|index| index.parse::<usize>().ok())
      .filter(|index| *index < self.doc_values_formats.len())
      .ok_or_else(|| {
        LuceneError::illegal_argument(format!("unexpected doc values field: {field}"))
      })?;
    Ok(&self.doc_values_formats[index])
  }
}

#[derive(Clone)]
pub struct MergePerFieldCodec {
  delegate: DefaultCodec,
  postings_format: MergePerFieldPostingsFormat,
  doc_values_format: MergePerFieldDocValuesFormat,
}

impl MergePerFieldCodec {
  pub(crate) fn new(barrier: Arc<Barrier>) -> Self {
    Self {
      delegate: TestUtil::get_default_codec(),
      postings_format: PerFieldPostingsFormat::new(MergePerFieldPostingsFormatBase {
        postings_formats: (0..18)
          .map(|_| {
            BlockingOnMergePostingsFormat::new(
              TestUtil::get_default_postings_format(),
              Arc::clone(&barrier),
            )
          })
          .collect(),
      }),
      doc_values_format: PerFieldDocValuesFormat::new(MergePerFieldDocValuesFormatBase {
        doc_values_formats: (0..18)
          .map(|_| {
            BlockingOnMergeDocValuesFormat::new(
              TestUtil::get_default_doc_values_format(),
              Arc::clone(&barrier),
            )
          })
          .collect(),
      }),
    }
  }
}

impl Display for MergePerFieldCodec {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "MergePerFieldCodec({})", self.delegate)
  }
}

impl Codec for MergePerFieldCodec {
  type PostingsFormat = MergePerFieldPostingsFormat;
  type DocValuesFormat = MergePerFieldDocValuesFormat;
  type StoredFieldsFormat = <DefaultCodec as Codec>::StoredFieldsFormat;
  type TermVectorsFormat = <DefaultCodec as Codec>::TermVectorsFormat;
  type FieldInfosFormat = <DefaultCodec as Codec>::FieldInfosFormat;
  type SegmentInfoFormat = <DefaultCodec as Codec>::SegmentInfoFormat;
  type NormsFormat = <DefaultCodec as Codec>::NormsFormat;
  type LiveDocsFormat = <DefaultCodec as Codec>::LiveDocsFormat;
  type CompoundFormat = <DefaultCodec as Codec>::CompoundFormat;
  type PointsFormat = <DefaultCodec as Codec>::PointsFormat;
  type KnnVectorsFormat = <DefaultCodec as Codec>::KnnVectorsFormat;

  fn postings_format(&self) -> Self::PostingsFormat {
    self.postings_format.clone()
  }

  fn doc_values_format(&self) -> Self::DocValuesFormat {
    self.doc_values_format.clone()
  }

  fn stored_fields_format(&self) -> Self::StoredFieldsFormat {
    self.delegate.stored_fields_format()
  }

  fn term_vectors_format(&self) -> Self::TermVectorsFormat {
    self.delegate.term_vectors_format()
  }

  fn field_infos_format(&self) -> Self::FieldInfosFormat {
    self.delegate.field_infos_format()
  }

  fn segment_info_format(&self) -> Self::SegmentInfoFormat {
    self.delegate.segment_info_format()
  }

  fn norms_format(&self) -> Self::NormsFormat {
    self.delegate.norms_format()
  }

  fn live_docs_format(&self) -> Self::LiveDocsFormat {
    self.delegate.live_docs_format()
  }

  fn compound_format(&self) -> Self::CompoundFormat {
    self.delegate.compound_format()
  }

  fn points_format(&self) -> Self::PointsFormat {
    self.delegate.points_format()
  }

  fn knn_vectors_format(&self) -> Result<Self::KnnVectorsFormat> {
    self.delegate.knn_vectors_format()
  }

  fn get_name(&self) -> &str {
    self.delegate.get_name()
  }
}

pub struct BlockingOnMergePostingsFormat {
  postings_format: DefaultPostingsFormat,
  barrier: Arc<Barrier>,
  identity: Identity,
}

impl BlockingOnMergePostingsFormat {
  fn new(postings_format: DefaultPostingsFormat, barrier: Arc<Barrier>) -> Self {
    Self {
      postings_format,
      barrier,
      identity: Identity::new(),
    }
  }
}

impl HasIdentity for BlockingOnMergePostingsFormat {
  fn identity(&self) -> &Identity {
    &self.identity
  }
}

impl PostingsFormat for BlockingOnMergePostingsFormat {
  fn get_name(&self) -> &str {
    self.postings_format.get_name()
  }

  type FieldsConsumer<O: IndexOutput> =
    BlockingOnMergeFieldsConsumer<<DefaultPostingsFormat as PostingsFormat>::FieldsConsumer<O>>;

  fn fields_consumer<D1, D2>(
    &self,
    state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::FieldsConsumer<D1::IndexOutput>>
  where
    D1: Directory,
  {
    Ok(BlockingOnMergeFieldsConsumer {
      in_: self.postings_format.fields_consumer(state, segment_info)?,
      barrier: Arc::clone(&self.barrier),
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
    self.postings_format.fields_producer(state, segment_info)
  }

  fn for_name(name: &str) -> Result<Arc<Self>> {
    DefaultPostingsFormat::for_name(name)?;
    Ok(Arc::new(Self::new(
      TestUtil::get_default_postings_format(),
      Arc::new(Barrier::new(1)),
    )))
  }
}

pub struct BlockingOnMergeFieldsConsumer<C> {
  in_: C,
  barrier: Arc<Barrier>,
}

impl<C> Closeable for BlockingOnMergeFieldsConsumer<C>
where
  C: FieldsConsumer,
{
  fn close(&mut self) -> Result<()> {
    self.in_.close()
  }
}

impl<C> FieldsConsumer for BlockingOnMergeFieldsConsumer<C>
where
  C: FieldsConsumer,
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
    self.in_.write(state, segment_info, fields, norms)
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
    self.barrier.wait();
    self.in_.merge(state, segment_info, merge_state, norms)
  }
}

pub struct BlockingOnMergeDocValuesFormat {
  doc_values_format: DefaultDocValuesFormat,
  barrier: Arc<Barrier>,
  identity: Identity,
}

impl BlockingOnMergeDocValuesFormat {
  fn new(doc_values_format: DefaultDocValuesFormat, barrier: Arc<Barrier>) -> Self {
    Self {
      doc_values_format,
      barrier,
      identity: Identity::new(),
    }
  }
}

impl Display for BlockingOnMergeDocValuesFormat {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    Display::fmt(&self.doc_values_format, f)
  }
}

impl HasIdentity for BlockingOnMergeDocValuesFormat {
  fn identity(&self) -> &Identity {
    &self.identity
  }
}

impl DocValuesFormat for BlockingOnMergeDocValuesFormat {
  fn get_name(&self) -> &str {
    self.doc_values_format.get_name()
  }

  type DocValuesConsumer<O: IndexOutput> = BlockingOnMergeDocValuesConsumer<
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
    Ok(BlockingOnMergeDocValuesConsumer {
      in_: self
        .doc_values_format
        .fields_consumer(state, segment_info)?,
      barrier: Arc::clone(&self.barrier),
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
    self.doc_values_format.fields_producer(state, segment_info)
  }

  fn for_name(name: &str) -> Result<Arc<Self>> {
    DefaultDocValuesFormat::for_name(name)?;
    Ok(Arc::new(Self::new(
      TestUtil::get_default_doc_values_format(),
      Arc::new(Barrier::new(1)),
    )))
  }
}

pub struct BlockingOnMergeDocValuesConsumer<C> {
  in_: C,
  barrier: Arc<Barrier>,
}

impl<C> Closeable for BlockingOnMergeDocValuesConsumer<C>
where
  C: DocValuesConsumer,
{
  fn close(&mut self) -> Result<()> {
    self.in_.close()
  }
}

impl<C> DocValuesConsumer for BlockingOnMergeDocValuesConsumer<C>
where
  C: DocValuesConsumer,
{
  type IndexOutput = C::IndexOutput;

  fn add_numeric_field<D1, D2, D>(
    &mut self,
    write_state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
    field: &Arc<FieldInfo>,
    values_producer: &D,
  ) -> Result<()>
  where
    D1: Directory<IndexOutput = Self::IndexOutput>,
    D: DocValuesProducer,
  {
    self
      .in_
      .add_numeric_field(write_state, segment_info, field, values_producer)
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
    D: DocValuesProducer,
  {
    self
      .in_
      .add_binary_field(write_state, segment_info, field, values_producer)
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
    D: DocValuesProducer,
  {
    self
      .in_
      .add_sorted_field(write_state, segment_info, field, values_producer)
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
    D: DocValuesProducer,
  {
    self
      .in_
      .add_sorted_numeric_field(write_state, segment_info, field, values_producer)
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
    D: DocValuesProducer,
  {
    self
      .in_
      .add_sorted_set_field(write_state, segment_info, field, values_producer)
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
    self.barrier.wait();
    self.in_.merge(write_state, segment_info, merge_state)
  }
}
