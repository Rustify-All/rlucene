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

#[cfg(test)]
use crate::core::codecs::Codec;
#[cfg(test)]
use crate::core::codecs::compound_format::CompoundFormat;
#[cfg(test)]
use crate::core::codecs::doc_values_consumer::DocValuesConsumerEnum2;
use crate::core::codecs::doc_values_format::DocValuesFormat;
use crate::core::codecs::doc_values_producer::DocValuesProducer;
#[cfg(test)]
use crate::core::codecs::dummy::dummy_mutable_point_tree::DummyMutablePointTree;
#[cfg(test)]
use crate::core::codecs::field_infos_format::FieldInfosFormat;
#[cfg(test)]
use crate::core::codecs::fields_consumer::FieldsConsumerEnum2;
#[cfg(test)]
use crate::core::codecs::knn_field_vectors_writer::VectorValueEnum;
use crate::core::codecs::knn_vectors_format::KnnVectorsFormat;
#[cfg(test)]
use crate::core::codecs::knn_vectors_formats::KnnVectorsFormatsReader;
#[cfg(test)]
use crate::core::codecs::knn_vectors_reader::KnnVectorsReader;
#[cfg(test)]
use crate::core::codecs::knn_vectors_writer::KnnVectorsWriter;
use crate::core::codecs::live_docs_format::LiveDocsFormat;
#[cfg(test)]
use crate::core::codecs::lucene90::compressing::lucene90_compressing_stored_fields_format::Lucene90CompressingStoredFieldsFormat;
#[cfg(test)]
use crate::core::codecs::lucene90::compressing::lucene90_compressing_stored_fields_reader::Lucene90CompressingStoredFieldsReader;
#[cfg(test)]
use crate::core::codecs::lucene90::compressing::lucene90_compressing_term_vectors_format::Lucene90CompressingTermVectorsFormat;
use crate::core::codecs::lucene90_live_docs_format::Lucene90LiveDocsFormat;
use crate::core::codecs::lucene90_norms_format::Lucene90NormsFormat;
use crate::core::codecs::lucene90_points_format::Lucene90PointsFormat;
use crate::core::codecs::lucene90_stored_fields_format::Lucene90StoredFieldsFormat;
use crate::core::codecs::lucene90_term_vectors_format::Lucene90TermVectorsFormat;
#[cfg(test)]
use crate::core::codecs::lucene101_codec::Lucene101Codec;
use crate::core::codecs::lucene101_codec::{
  Lucene101CodecDocValuesFormat, Lucene101CodecKnnVectorsFormat, Lucene101CodecPostingsFormat,
};
#[cfg(test)]
use crate::core::codecs::norms_consumer::NormsConsumerEnum2;
use crate::core::codecs::norms_format::NormsFormat;
use crate::core::codecs::norms_producer::NormsProducer;
use crate::core::codecs::points_format::PointsFormat;
#[cfg(test)]
use crate::core::codecs::points_reader::PointsReader;
#[cfg(test)]
use crate::core::codecs::points_writer::PointsWriter;
use crate::core::codecs::postings_format::PostingsFormat;
#[cfg(test)]
use crate::core::codecs::segment_info_format::SegmentInfoFormat;
use crate::core::codecs::stored_fields_format::StoredFieldsFormat;
#[cfg(test)]
use crate::core::codecs::stored_fields_reader::{DefaultStoredFieldsReader, StoredFieldsReader};
#[cfg(test)]
use crate::core::codecs::stored_fields_writer::{StoredFieldsWriter, StoredFieldsWriterEnum2};
use crate::core::codecs::term_vectors_format::TermVectorsFormat;
#[cfg(test)]
use crate::core::codecs::term_vectors_reader::TermVectorsReaderEnum2;
#[cfg(test)]
use crate::core::codecs::term_vectors_writer::TermVectorsWriterEnum2;
#[cfg(test)]
use crate::core::document::document::Document;
#[cfg(test)]
use crate::core::index::codec_reader::CodecReader;
#[cfg(test)]
use crate::core::index::doc_values_iterator::DocValuesIterator;
#[cfg(test)]
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::index_reader::Identity;
#[cfg(test)]
use crate::core::index::knn_vector_values::{DocIndexIterator, KnnVectorValues};
#[cfg(test)]
use crate::core::index::merge_state::MergeState;
#[cfg(test)]
use crate::core::index::numeric_doc_values::NumericDocValues;
#[cfg(test)]
use crate::core::index::point_values::{PointTreeEnum, PointTreeEnum2, PointValues};
use crate::core::index::segment_commit_info::SegmentCommitInfo;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::index::segment_write_state::SegmentWriteState;
#[cfg(test)]
use crate::core::index::sorter::DocMap;
#[cfg(test)]
use crate::core::index::stored_field_visitor::StoredFieldVisitor;
#[cfg(test)]
use crate::core::index::stored_fields::{RawStoredFieldsReader, StoredFields};
#[cfg(test)]
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
#[cfg(test)]
use crate::core::search::vector_scorer::VectorScorer;
use crate::core::store::directory::Directory;
use crate::core::store::{IOContext, IndexInput, IndexOutput};
use crate::core::util::HasIdentity;
#[cfg(test)]
use crate::core::util::StringHelper;
#[cfg(test)]
use crate::core::util::accountable::Accountable;
use crate::core::util::bits::Bits;
#[cfg(test)]
use crate::core::util::clone::TryClone;
#[cfg(test)]
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, Result};
#[cfg(test)]
use crate::core::util::fixed_bit_set::FixedBitSet;
#[cfg(test)]
use crate::core::util::hnsw::hnsw_graph::{HnswGraph, NodesIterator};
#[cfg(test)]
use crate::core::util::hnsw::neighbor_array::NeighborArray;
#[cfg(test)]
use crate::core::{
  index::byte_vector_values::ByteVectorValues, index::float_vector_values::FloatVectorValues,
};
#[cfg(test)]
use crate::test_framework::core::codecs::asserting_codec::{
  AssertingCodec, AssertingCodecFieldsProducer,
};
#[cfg(test)]
use crate::test_framework::core::codecs::asserting_doc_values_format::AssertingDocValuesProducer;
#[cfg(test)]
use crate::test_framework::core::codecs::asserting_stored_fields_format::AssertingStoredFieldsReader;
#[cfg(test)]
use crate::test_framework::core::codecs::cranky::cranky_codec::CrankyCodec;
#[cfg(test)]
use crate::test_framework::core::codecs::lucene90::test_lucene90_points_format::TestLucene90PointsFormatPointsFormat;
#[cfg(test)]
use crate::test_framework::core::geo::random_distance_codec::RandomDistanceCodec;
#[cfg(test)]
use crate::test_framework::core::index::base_postings_format_test_case::{
  InvertedWriteFieldsConsumer, InvertedWritePostingsFormat,
};
#[cfg(test)]
use crate::test_framework::core::index::test_index_sorting::AssertingNeedsIndexSortCodec;
#[cfg(test)]
use crate::test_framework::core::index::test_index_writer_force_merge::{
  MergePerFieldCodec, MergePerFieldDocValuesFormat, MergePerFieldPostingsFormat,
};
#[cfg(test)]
use std::borrow::Cow;
use std::collections::HashSet;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

#[cfg(test)]
type AssertingPostingsFormat = <AssertingCodec as Codec>::PostingsFormat;
#[cfg(test)]
type AssertingDocValuesFormat = <AssertingCodec as Codec>::DocValuesFormat;
#[cfg(test)]
type AssertingStoredFieldsFormat = <AssertingCodec as Codec>::StoredFieldsFormat;
#[cfg(test)]
type AssertingTermVectorsFormat = <AssertingCodec as Codec>::TermVectorsFormat;
#[cfg(test)]
type AssertingNormsFormat = <AssertingCodec as Codec>::NormsFormat;
#[cfg(test)]
type AssertingLiveDocsFormat = <AssertingCodec as Codec>::LiveDocsFormat;
#[cfg(test)]
type AssertingPointsFormat = <AssertingCodec as Codec>::PointsFormat;
#[cfg(test)]
type AssertingKnnVectorsFormat = <AssertingCodec as Codec>::KnnVectorsFormat;
#[cfg(test)]
type AssertingNeedsIndexSortPointsFormat = <AssertingNeedsIndexSortCodec as Codec>::PointsFormat;
#[cfg(test)]
type RandomDistancePointsFormat = <RandomDistanceCodec as Codec>::PointsFormat;
#[cfg(test)]
type CrankyLucene101Codec = CrankyCodec<Lucene101Codec>;
#[cfg(test)]
type CrankyAssertingCodec = CrankyCodec<AssertingCodec>;
#[cfg(test)]
type CrankyLucene101PostingsFormat = <CrankyLucene101Codec as Codec>::PostingsFormat;
#[cfg(test)]
type CrankyAssertingPostingsFormat = <CrankyAssertingCodec as Codec>::PostingsFormat;
#[cfg(test)]
type CrankyLucene101DocValuesFormat = <CrankyLucene101Codec as Codec>::DocValuesFormat;
#[cfg(test)]
type CrankyAssertingDocValuesFormat = <CrankyAssertingCodec as Codec>::DocValuesFormat;
#[cfg(test)]
type MergePerFieldCodecPostingsFormat = <MergePerFieldCodec as Codec>::PostingsFormat;
#[cfg(test)]
type MergePerFieldCodecDocValuesFormat = <MergePerFieldCodec as Codec>::DocValuesFormat;
#[cfg(test)]
type CrankyLucene101StoredFieldsFormat = <CrankyLucene101Codec as Codec>::StoredFieldsFormat;
#[cfg(test)]
type CrankyAssertingStoredFieldsFormat = <CrankyAssertingCodec as Codec>::StoredFieldsFormat;
#[cfg(test)]
type CrankyLucene101TermVectorsFormat = <CrankyLucene101Codec as Codec>::TermVectorsFormat;
#[cfg(test)]
type CrankyAssertingTermVectorsFormat = <CrankyAssertingCodec as Codec>::TermVectorsFormat;
#[cfg(test)]
type CrankyLucene101NormsFormat = <CrankyLucene101Codec as Codec>::NormsFormat;
#[cfg(test)]
type CrankyAssertingNormsFormat = <CrankyAssertingCodec as Codec>::NormsFormat;
#[cfg(test)]
type CrankyLucene101LiveDocsFormat = <CrankyLucene101Codec as Codec>::LiveDocsFormat;
#[cfg(test)]
type CrankyAssertingLiveDocsFormat = <CrankyAssertingCodec as Codec>::LiveDocsFormat;
#[cfg(test)]
type CrankyLucene101PointsFormat = <CrankyLucene101Codec as Codec>::PointsFormat;
#[cfg(test)]
type CrankyAssertingPointsFormat = <CrankyAssertingCodec as Codec>::PointsFormat;

pub enum CodecPostingsFormat {
  Lucene101(Lucene101CodecPostingsFormat),
  #[cfg(test)]
  Asserting(AssertingPostingsFormat),
  #[cfg(test)]
  MergePerField(MergePerFieldPostingsFormat),
  #[cfg(test)]
  CrankyLucene101(CrankyLucene101PostingsFormat),
  #[cfg(test)]
  CrankyAsserting(CrankyAssertingPostingsFormat),
  #[cfg(test)]
  InvertedWrite(InvertedWritePostingsFormat),
}

pub enum CodecDocValuesFormat {
  Lucene101(Lucene101CodecDocValuesFormat),
  #[cfg(test)]
  Asserting(AssertingDocValuesFormat),
  #[cfg(test)]
  MergePerField(MergePerFieldDocValuesFormat),
  #[cfg(test)]
  CrankyLucene101(CrankyLucene101DocValuesFormat),
  #[cfg(test)]
  CrankyAsserting(CrankyAssertingDocValuesFormat),
}

pub enum CodecStoredFieldsFormat {
  Lucene90(Lucene90StoredFieldsFormat),
  #[cfg(test)]
  Compressing(Lucene90CompressingStoredFieldsFormat),
  #[cfg(test)]
  Asserting(AssertingStoredFieldsFormat),
  #[cfg(test)]
  CrankyLucene101(CrankyLucene101StoredFieldsFormat),
  #[cfg(test)]
  CrankyAsserting(CrankyAssertingStoredFieldsFormat),
}

pub enum CodecTermVectorsFormat {
  Lucene90(Lucene90TermVectorsFormat),
  #[cfg(test)]
  Compressing(Lucene90CompressingTermVectorsFormat),
  #[cfg(test)]
  Asserting(AssertingTermVectorsFormat),
  #[cfg(test)]
  CrankyLucene101(CrankyLucene101TermVectorsFormat),
  #[cfg(test)]
  CrankyAsserting(CrankyAssertingTermVectorsFormat),
}

pub enum CodecNormsFormat {
  Lucene90(Lucene90NormsFormat),
  #[cfg(test)]
  Asserting(AssertingNormsFormat),
  #[cfg(test)]
  CrankyLucene101(CrankyLucene101NormsFormat),
  #[cfg(test)]
  CrankyAsserting(CrankyAssertingNormsFormat),
}

pub enum CodecLiveDocsFormat {
  Lucene90(Lucene90LiveDocsFormat),
  #[cfg(test)]
  Asserting(AssertingLiveDocsFormat),
  #[cfg(test)]
  CrankyLucene101(CrankyLucene101LiveDocsFormat),
  #[cfg(test)]
  CrankyAsserting(CrankyAssertingLiveDocsFormat),
}

pub enum CodecPointsFormat {
  Lucene90(Lucene90PointsFormat),
  #[cfg(test)]
  TestLucene90(TestLucene90PointsFormatPointsFormat),
  #[cfg(test)]
  Asserting(AssertingPointsFormat),
  #[cfg(test)]
  AssertingNeedsIndexSort(AssertingNeedsIndexSortPointsFormat),
  #[cfg(test)]
  RandomDistance(RandomDistancePointsFormat),
  #[cfg(test)]
  CrankyLucene101(CrankyLucene101PointsFormat),
  #[cfg(test)]
  CrankyAsserting(CrankyAssertingPointsFormat),
}

pub enum CodecKnnVectorsFormat {
  Lucene101(Lucene101CodecKnnVectorsFormat),
  #[cfg(test)]
  Asserting(AssertingKnnVectorsFormat),
}

#[cfg(test)]
pub enum CodecFieldInfosFormat {
  Lucene101(<Lucene101Codec as Codec>::FieldInfosFormat),
  Cranky(<CrankyLucene101Codec as Codec>::FieldInfosFormat),
}

#[cfg(test)]
impl FieldInfosFormat for CodecFieldInfosFormat {
  fn read<D>(
    &self,
    directory: &impl Directory,
    segment_info: &SegmentInfo<D>,
    segment_suffix: &str,
    io_context: &IOContext,
  ) -> Result<FieldInfos> {
    match self {
      Self::Lucene101(format) => format.read(directory, segment_info, segment_suffix, io_context),
      Self::Cranky(format) => format.read(directory, segment_info, segment_suffix, io_context),
    }
  }

  fn write<D>(
    &self,
    directory: &impl Directory,
    segment_info: &SegmentInfo<D>,
    segment_suffix: &str,
    infos: &FieldInfos,
    io_context: &IOContext,
  ) -> Result<()> {
    match self {
      Self::Lucene101(format) => {
        format.write(directory, segment_info, segment_suffix, infos, io_context)
      },
      Self::Cranky(format) => {
        format.write(directory, segment_info, segment_suffix, infos, io_context)
      },
    }
  }
}

#[cfg(test)]
pub enum CodecSegmentInfoFormat {
  Lucene101(<Lucene101Codec as Codec>::SegmentInfoFormat),
  Cranky(<CrankyLucene101Codec as Codec>::SegmentInfoFormat),
}

#[cfg(test)]
impl SegmentInfoFormat for CodecSegmentInfoFormat {
  fn read<D>(
    &self,
    directory: Arc<D>,
    segment_name: &str,
    segment_id: &[u8; StringHelper::ID_LENGTH],
    context: &IOContext,
  ) -> Result<SegmentInfo<D>>
  where
    D: Directory,
  {
    match self {
      Self::Lucene101(format) => format.read(directory, segment_name, segment_id, context),
      Self::Cranky(format) => format.read(directory, segment_name, segment_id, context),
    }
  }

  fn write<D>(
    &self,
    directory: &impl Directory,
    info: &mut SegmentInfo<D>,
    context: &IOContext,
  ) -> Result<()> {
    match self {
      Self::Lucene101(format) => format.write(directory, info, context),
      Self::Cranky(format) => format.write(directory, info, context),
    }
  }
}

#[cfg(test)]
pub enum CodecCompoundFormat {
  Lucene101(<Lucene101Codec as Codec>::CompoundFormat),
  Cranky(<CrankyLucene101Codec as Codec>::CompoundFormat),
}

#[cfg(test)]
impl CompoundFormat for CodecCompoundFormat {
  type Directory<D>
    = <<Lucene101Codec as Codec>::CompoundFormat as CompoundFormat>::Directory<D>
  where
    D: Directory;

  fn get_compound_reader<D>(&self, dir: &D, si: &SegmentInfo<D>) -> Result<Self::Directory<D>>
  where
    D: Directory,
  {
    match self {
      Self::Lucene101(format) => format.get_compound_reader(dir, si),
      Self::Cranky(format) => format.get_compound_reader(dir, si),
    }
  }

  fn write<D>(&self, dir: &impl Directory, si: &SegmentInfo<D>, context: &IOContext) -> Result<()> {
    match self {
      Self::Lucene101(format) => format.write(dir, si, context),
      Self::Cranky(format) => format.write(dir, si, context),
    }
  }
}

#[cfg(not(test))]
pub type CodecFieldsConsumer<O> =
  <Lucene101CodecPostingsFormat as PostingsFormat>::FieldsConsumer<O>;
#[cfg(test)]
pub type BaseCodecFieldsConsumer<O> = FieldsConsumerEnum2<
  FieldsConsumerEnum2<
    FieldsConsumerEnum2<
      <Lucene101CodecPostingsFormat as PostingsFormat>::FieldsConsumer<O>,
      <AssertingPostingsFormat as PostingsFormat>::FieldsConsumer<O>,
    >,
    FieldsConsumerEnum2<
      <CrankyLucene101PostingsFormat as PostingsFormat>::FieldsConsumer<O>,
      <CrankyAssertingPostingsFormat as PostingsFormat>::FieldsConsumer<O>,
    >,
  >,
  <MergePerFieldCodecPostingsFormat as PostingsFormat>::FieldsConsumer<O>,
>;
#[cfg(test)]
pub type CodecFieldsConsumer<O> = FieldsConsumerEnum2<
  BaseCodecFieldsConsumer<O>,
  InvertedWriteFieldsConsumer<BaseCodecFieldsConsumer<O>>,
>;

#[cfg(not(test))]
pub type CodecFieldsProducer<I> =
  <Lucene101CodecPostingsFormat as PostingsFormat>::FieldsProducer<I>;
#[cfg(test)]
pub type BaseCodecFieldsProducer<I> =
  <AssertingPostingsFormat as PostingsFormat>::FieldsProducer<I>;
#[cfg(test)]
pub type CodecFieldsProducer<I> = BaseCodecFieldsProducer<I>;

#[cfg(test)]
impl CodecPostingsFormat {
  pub(crate) fn base_fields_consumer<D1, D2>(
    &self,
    state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<BaseCodecFieldsConsumer<D1::IndexOutput>>
  where
    D1: Directory,
  {
    match self {
      Self::Lucene101(format) => format.fields_consumer(state, segment_info).map(|consumer| {
        FieldsConsumerEnum2::A(FieldsConsumerEnum2::A(FieldsConsumerEnum2::A(consumer)))
      }),
      Self::Asserting(format) => format.fields_consumer(state, segment_info).map(|consumer| {
        FieldsConsumerEnum2::A(FieldsConsumerEnum2::A(FieldsConsumerEnum2::B(consumer)))
      }),
      Self::MergePerField(format) => format
        .fields_consumer(state, segment_info)
        .map(FieldsConsumerEnum2::B),
      Self::CrankyLucene101(format) => {
        format.fields_consumer(state, segment_info).map(|consumer| {
          FieldsConsumerEnum2::A(FieldsConsumerEnum2::B(FieldsConsumerEnum2::A(consumer)))
        })
      },
      Self::CrankyAsserting(format) => {
        format.fields_consumer(state, segment_info).map(|consumer| {
          FieldsConsumerEnum2::A(FieldsConsumerEnum2::B(FieldsConsumerEnum2::B(consumer)))
        })
      },
      Self::InvertedWrite(_) => Err(LuceneError::illegal_state(
        "InvertedWritePostingsFormat cannot wrap itself",
      )),
    }
  }

  pub(crate) fn base_fields_producer<D1, D2>(
    &self,
    state: &SegmentReadState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<BaseCodecFieldsProducer<D1::IndexInput>>
  where
    D1: Directory,
  {
    match self {
      Self::Lucene101(format) => format
        .fields_producer(state, segment_info)
        .and_then(|reader| reader.map_producers(AssertingCodecFieldsProducer::default)),
      Self::Asserting(format) => format.fields_producer(state, segment_info),
      Self::MergePerField(format) => format
        .fields_producer(state, segment_info)
        .and_then(|reader| reader.map_producers(AssertingCodecFieldsProducer::default)),
      Self::CrankyLucene101(format) => format
        .fields_producer(state, segment_info)
        .and_then(|reader| reader.map_producers(AssertingCodecFieldsProducer::default)),
      Self::CrankyAsserting(format) => format.fields_producer(state, segment_info),
      Self::InvertedWrite(_) => Err(LuceneError::illegal_state(
        "InvertedWritePostingsFormat cannot wrap itself",
      )),
    }
  }
}

impl HasIdentity for CodecPostingsFormat {
  fn identity(&self) -> &Identity {
    match self {
      Self::Lucene101(format) => format.identity(),
      #[cfg(test)]
      Self::Asserting(format) => format.identity(),
      #[cfg(test)]
      Self::MergePerField(format) => format.identity(),
      #[cfg(test)]
      Self::CrankyLucene101(format) => format.identity(),
      #[cfg(test)]
      Self::CrankyAsserting(format) => format.identity(),
      #[cfg(test)]
      Self::InvertedWrite(format) => format.identity(),
    }
  }
}

impl PostingsFormat for CodecPostingsFormat {
  fn get_name(&self) -> &str {
    match self {
      Self::Lucene101(format) => format.get_name(),
      #[cfg(test)]
      Self::Asserting(format) => format.get_name(),
      #[cfg(test)]
      Self::MergePerField(format) => format.get_name(),
      #[cfg(test)]
      Self::CrankyLucene101(format) => format.get_name(),
      #[cfg(test)]
      Self::CrankyAsserting(format) => format.get_name(),
      #[cfg(test)]
      Self::InvertedWrite(format) => format.get_name(),
    }
  }

  type FieldsConsumer<O: IndexOutput> = CodecFieldsConsumer<O>;

  fn fields_consumer<D1, D2>(
    &self,
    state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::FieldsConsumer<D1::IndexOutput>>
  where
    D1: Directory,
  {
    #[cfg(not(test))]
    {
      match self {
        Self::Lucene101(format) => format.fields_consumer(state, segment_info),
      }
    }
    #[cfg(test)]
    {
      match self {
        Self::InvertedWrite(format) => format
          .fields_consumer(state, segment_info)
          .map(FieldsConsumerEnum2::B),
        _ => self
          .base_fields_consumer(state, segment_info)
          .map(FieldsConsumerEnum2::A),
      }
    }
  }

  type FieldsProducer<I: IndexInput> = CodecFieldsProducer<I>;

  fn fields_producer<D1, D2>(
    &self,
    state: &SegmentReadState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::FieldsProducer<D1::IndexInput>>
  where
    D1: Directory,
  {
    #[cfg(not(test))]
    {
      match self {
        Self::Lucene101(format) => format.fields_producer(state, segment_info),
      }
    }
    #[cfg(test)]
    {
      match self {
        Self::InvertedWrite(format) => format.fields_producer(state, segment_info),
        _ => self.base_fields_producer(state, segment_info),
      }
    }
  }

  fn for_name(name: &str) -> Result<Arc<Self>> {
    Err(LuceneError::illegal_argument(format!(
      "Could not load postings format named \"{name}\""
    )))
  }
}

#[cfg(not(test))]
pub type CodecDocValuesConsumer<O> =
  <Lucene101CodecDocValuesFormat as DocValuesFormat>::DocValuesConsumer<O>;
#[cfg(test)]
pub type CodecDocValuesConsumer<O> = DocValuesConsumerEnum2<
  DocValuesConsumerEnum2<
    DocValuesConsumerEnum2<
      <Lucene101CodecDocValuesFormat as DocValuesFormat>::DocValuesConsumer<O>,
      <AssertingDocValuesFormat as DocValuesFormat>::DocValuesConsumer<O>,
    >,
    DocValuesConsumerEnum2<
      <CrankyLucene101DocValuesFormat as DocValuesFormat>::DocValuesConsumer<O>,
      <CrankyAssertingDocValuesFormat as DocValuesFormat>::DocValuesConsumer<O>,
    >,
  >,
  <MergePerFieldCodecDocValuesFormat as DocValuesFormat>::DocValuesConsumer<O>,
>;

#[cfg(not(test))]
pub type CodecDocValuesProducer<I> =
  <Lucene101CodecDocValuesFormat as DocValuesFormat>::DocValuesProducer<I>;
#[cfg(test)]
pub type CodecDocValuesProducer<I> =
  <AssertingDocValuesFormat as DocValuesFormat>::DocValuesProducer<I>;

pub type CodecNumericDocValues<I> =
  <CodecDocValuesProducer<I> as DocValuesProducer>::NumericDocValues;
pub type CodecBinaryDocValues<I> =
  <CodecDocValuesProducer<I> as DocValuesProducer>::BinaryDocValues;
pub type CodecSortedDocValues<I> =
  <CodecDocValuesProducer<I> as DocValuesProducer>::SortedDocValues;
pub type CodecSortedNumericDocValues<I> =
  <CodecDocValuesProducer<I> as DocValuesProducer>::SortedNumericDocValues;
pub type CodecSortedSetDocValues<I> =
  <CodecDocValuesProducer<I> as DocValuesProducer>::SortedSetDocValues;
pub type CodecDocValuesSkipper<I> =
  <CodecDocValuesProducer<I> as DocValuesProducer>::DocValuesSkipper;

impl Display for CodecDocValuesFormat {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Lucene101(format) => Display::fmt(format, f),
      #[cfg(test)]
      Self::Asserting(format) => Display::fmt(format, f),
      #[cfg(test)]
      Self::MergePerField(format) => Display::fmt(format, f),
      #[cfg(test)]
      Self::CrankyLucene101(format) => Display::fmt(format, f),
      #[cfg(test)]
      Self::CrankyAsserting(format) => Display::fmt(format, f),
    }
  }
}

impl HasIdentity for CodecDocValuesFormat {
  fn identity(&self) -> &Identity {
    match self {
      Self::Lucene101(format) => format.identity(),
      #[cfg(test)]
      Self::Asserting(format) => format.identity(),
      #[cfg(test)]
      Self::MergePerField(format) => format.identity(),
      #[cfg(test)]
      Self::CrankyLucene101(format) => format.identity(),
      #[cfg(test)]
      Self::CrankyAsserting(format) => format.identity(),
    }
  }
}

impl DocValuesFormat for CodecDocValuesFormat {
  fn get_name(&self) -> &str {
    match self {
      Self::Lucene101(format) => format.get_name(),
      #[cfg(test)]
      Self::Asserting(format) => format.get_name(),
      #[cfg(test)]
      Self::MergePerField(format) => format.get_name(),
      #[cfg(test)]
      Self::CrankyLucene101(format) => format.get_name(),
      #[cfg(test)]
      Self::CrankyAsserting(format) => format.get_name(),
    }
  }

  type DocValuesConsumer<O: IndexOutput> = CodecDocValuesConsumer<O>;

  fn fields_consumer<D1, D2>(
    &self,
    state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::DocValuesConsumer<D1::IndexOutput>>
  where
    D1: Directory,
  {
    match self {
      Self::Lucene101(format) => {
        #[cfg(not(test))]
        {
          format.fields_consumer(state, segment_info)
        }
        #[cfg(test)]
        {
          format.fields_consumer(state, segment_info).map(|consumer| {
            DocValuesConsumerEnum2::A(DocValuesConsumerEnum2::A(DocValuesConsumerEnum2::A(
              consumer,
            )))
          })
        }
      },
      #[cfg(test)]
      Self::Asserting(format) => format.fields_consumer(state, segment_info).map(|consumer| {
        DocValuesConsumerEnum2::A(DocValuesConsumerEnum2::A(DocValuesConsumerEnum2::B(
          consumer,
        )))
      }),
      #[cfg(test)]
      Self::MergePerField(format) => format
        .fields_consumer(state, segment_info)
        .map(DocValuesConsumerEnum2::B),
      #[cfg(test)]
      Self::CrankyLucene101(format) => {
        format.fields_consumer(state, segment_info).map(|consumer| {
          DocValuesConsumerEnum2::A(DocValuesConsumerEnum2::B(DocValuesConsumerEnum2::A(
            consumer,
          )))
        })
      },
      #[cfg(test)]
      Self::CrankyAsserting(format) => {
        format.fields_consumer(state, segment_info).map(|consumer| {
          DocValuesConsumerEnum2::A(DocValuesConsumerEnum2::B(DocValuesConsumerEnum2::B(
            consumer,
          )))
        })
      },
    }
  }

  type DocValuesProducer<I: IndexInput> = CodecDocValuesProducer<I>;

  fn fields_producer<D1, D2>(
    &self,
    state: &SegmentReadState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::DocValuesProducer<D1::IndexInput>>
  where
    D1: Directory,
  {
    match self {
      Self::Lucene101(format) => {
        #[cfg(not(test))]
        {
          format.fields_producer(state, segment_info)
        }
        #[cfg(test)]
        {
          format
            .fields_producer(state, segment_info)
            .and_then(|reader| reader.map_producers(AssertingDocValuesProducer::new_default))
        }
      },
      #[cfg(test)]
      Self::Asserting(format) => format.fields_producer(state, segment_info),
      #[cfg(test)]
      Self::MergePerField(format) => format
        .fields_producer(state, segment_info)
        .and_then(|reader| reader.map_producers(AssertingDocValuesProducer::new_default)),
      #[cfg(test)]
      Self::CrankyLucene101(format) => format
        .fields_producer(state, segment_info)
        .and_then(|reader| reader.map_producers(AssertingDocValuesProducer::new_default)),
      #[cfg(test)]
      Self::CrankyAsserting(format) => format.fields_producer(state, segment_info),
    }
  }

  fn for_name(name: &str) -> Result<Arc<Self>> {
    Err(LuceneError::illegal_argument(format!(
      "Could not load doc values format named \"{name}\""
    )))
  }
}

#[cfg(not(test))]
pub type CodecStoredFieldsReader<I> =
  <Lucene90StoredFieldsFormat as StoredFieldsFormat>::StoredFieldsReader<I>;
#[cfg(test)]
pub enum CodecStoredFieldsReader<I: IndexInput> {
  Lucene90(Lucene90CompressingStoredFieldsReader<I>),
  Asserting(AssertingStoredFieldsReader<Lucene90CompressingStoredFieldsReader<I>>),
}

#[cfg(test)]
impl<I: IndexInput> CloseableRef for CodecStoredFieldsReader<I> {
  fn close(&self) -> Result<()> {
    match self {
      Self::Lucene90(reader) => reader.close(),
      Self::Asserting(reader) => reader.close(),
    }
  }
}

#[cfg(test)]
impl<I: IndexInput> RawStoredFieldsReader for CodecStoredFieldsReader<I> {
  type IndexInput = I;

  fn raw_stored_fields_mut(&mut self) -> Result<&mut DefaultStoredFieldsReader<Self::IndexInput>> {
    match self {
      Self::Lucene90(reader) => reader.raw_stored_fields_mut(),
      Self::Asserting(reader) => reader.raw_stored_fields_mut(),
    }
  }

  fn raw_stored_fields(&self) -> Result<&DefaultStoredFieldsReader<Self::IndexInput>> {
    match self {
      Self::Lucene90(reader) => reader.raw_stored_fields(),
      Self::Asserting(reader) => reader.raw_stored_fields(),
    }
  }
}

#[cfg(test)]
impl<I: IndexInput> StoredFields for CodecStoredFieldsReader<I> {
  fn prefetch(&mut self, doc_id: i32) -> Result<()> {
    match self {
      Self::Lucene90(reader) => reader.prefetch(doc_id),
      Self::Asserting(reader) => reader.prefetch(doc_id),
    }
  }

  fn document(&mut self, doc_id: i32) -> Result<Document> {
    match self {
      Self::Lucene90(reader) => reader.document(doc_id),
      Self::Asserting(reader) => reader.document(doc_id),
    }
  }

  fn document_with_visitor<W: StoredFieldsWriter>(
    &mut self,
    doc_id: i32,
    visitor: &mut impl StoredFieldVisitor,
    writer: Option<&mut W>,
  ) -> Result<()> {
    match self {
      Self::Lucene90(reader) => reader.document_with_visitor(doc_id, visitor, writer),
      Self::Asserting(reader) => reader.document_with_visitor(doc_id, visitor, writer),
    }
  }

  fn document_with_fields(
    &mut self,
    doc_id: i32,
    fields_to_load: &HashSet<String>,
  ) -> Result<Document> {
    match self {
      Self::Lucene90(reader) => reader.document_with_fields(doc_id, fields_to_load),
      Self::Asserting(reader) => reader.document_with_fields(doc_id, fields_to_load),
    }
  }
}

#[cfg(test)]
impl<I: IndexInput> TryClone for CodecStoredFieldsReader<I> {
  fn try_clone(&self) -> Result<Self> {
    match self {
      Self::Lucene90(reader) => reader.try_clone().map(Self::Lucene90),
      Self::Asserting(reader) => reader.try_clone().map(Self::Asserting),
    }
  }
}

#[cfg(test)]
impl<I: IndexInput> StoredFieldsReader for CodecStoredFieldsReader<I> {
  fn check_integrity(&self) -> Result<()> {
    match self {
      Self::Lucene90(reader) => reader.check_integrity(),
      Self::Asserting(reader) => reader.check_integrity(),
    }
  }

  fn get_merge_instance(&self) -> Result<Option<Self>> {
    match self {
      Self::Lucene90(reader) => reader
        .get_merge_instance()
        .map(|reader| reader.map(Self::Lucene90)),
      Self::Asserting(reader) => reader
        .get_merge_instance()
        .map(|reader| reader.map(Self::Asserting)),
    }
  }
}

#[cfg(not(test))]
pub type CodecStoredFieldsWriter<D> =
  <Lucene90StoredFieldsFormat as StoredFieldsFormat>::StoredFieldsWriter<D>;
#[cfg(test)]
pub type CodecStoredFieldsWriter<D> = StoredFieldsWriterEnum2<
  StoredFieldsWriterEnum2<
    <Lucene90StoredFieldsFormat as StoredFieldsFormat>::StoredFieldsWriter<D>,
    <AssertingStoredFieldsFormat as StoredFieldsFormat>::StoredFieldsWriter<D>,
  >,
  StoredFieldsWriterEnum2<
    <CrankyLucene101StoredFieldsFormat as StoredFieldsFormat>::StoredFieldsWriter<D>,
    <CrankyAssertingStoredFieldsFormat as StoredFieldsFormat>::StoredFieldsWriter<D>,
  >,
>;

impl StoredFieldsFormat for CodecStoredFieldsFormat {
  type StoredFieldsReader<I: IndexInput> = CodecStoredFieldsReader<I>;

  fn fields_reader<D1, D2>(
    &self,
    directory: &D1,
    segment_info: &SegmentInfo<D2>,
    field_infos: Arc<FieldInfos>,
    context: &IOContext,
  ) -> Result<Self::StoredFieldsReader<D1::IndexInput>>
  where
    D1: Directory,
  {
    match self {
      Self::Lucene90(format) => {
        #[cfg(not(test))]
        {
          format.fields_reader(directory, segment_info, field_infos, context)
        }
        #[cfg(test)]
        {
          format
            .fields_reader(directory, segment_info, field_infos, context)
            .map(CodecStoredFieldsReader::Lucene90)
        }
      },
      #[cfg(test)]
      Self::Compressing(format) => format
        .fields_reader(directory, segment_info, field_infos, context)
        .map(CodecStoredFieldsReader::Lucene90),
      #[cfg(test)]
      Self::Asserting(format) => format
        .fields_reader(directory, segment_info, field_infos, context)
        .map(CodecStoredFieldsReader::Asserting),
      #[cfg(test)]
      Self::CrankyLucene101(format) => format
        .fields_reader(directory, segment_info, field_infos, context)
        .map(CodecStoredFieldsReader::Lucene90),
      #[cfg(test)]
      Self::CrankyAsserting(format) => format
        .fields_reader(directory, segment_info, field_infos, context)
        .map(CodecStoredFieldsReader::Asserting),
    }
  }

  type StoredFieldsWriter<D: Directory> = CodecStoredFieldsWriter<D>;

  fn fields_writer<D1, D2>(
    &self,
    directory: D1,
    segment_info: &mut SegmentInfo<D2>,
    context: &IOContext,
  ) -> Result<Self::StoredFieldsWriter<D1>>
  where
    D1: Directory,
  {
    match self {
      Self::Lucene90(format) => {
        #[cfg(not(test))]
        {
          format.fields_writer(directory, segment_info, context)
        }
        #[cfg(test)]
        {
          format
            .fields_writer(directory, segment_info, context)
            .map(|writer| StoredFieldsWriterEnum2::A(StoredFieldsWriterEnum2::A(writer)))
        }
      },
      #[cfg(test)]
      Self::Compressing(format) => format
        .fields_writer(directory, segment_info, context)
        .map(|writer| StoredFieldsWriterEnum2::A(StoredFieldsWriterEnum2::A(writer))),
      #[cfg(test)]
      Self::Asserting(format) => format
        .fields_writer(directory, segment_info, context)
        .map(|writer| StoredFieldsWriterEnum2::A(StoredFieldsWriterEnum2::B(writer))),
      #[cfg(test)]
      Self::CrankyLucene101(format) => format
        .fields_writer(directory, segment_info, context)
        .map(|writer| StoredFieldsWriterEnum2::B(StoredFieldsWriterEnum2::A(writer))),
      #[cfg(test)]
      Self::CrankyAsserting(format) => format
        .fields_writer(directory, segment_info, context)
        .map(|writer| StoredFieldsWriterEnum2::B(StoredFieldsWriterEnum2::B(writer))),
    }
  }
}

#[cfg(not(test))]
pub type CodecTermVectorsReader<I> =
  <Lucene90TermVectorsFormat as TermVectorsFormat>::TermVectorsReader<I>;
#[cfg(test)]
pub type CodecTermVectorsReader<I> = TermVectorsReaderEnum2<
  <Lucene90TermVectorsFormat as TermVectorsFormat>::TermVectorsReader<I>,
  <AssertingTermVectorsFormat as TermVectorsFormat>::TermVectorsReader<I>,
>;
#[cfg(not(test))]
pub type CodecTermVectorsWriter<D> =
  <Lucene90TermVectorsFormat as TermVectorsFormat>::TermVectorsWriter<D>;
#[cfg(test)]
pub type CodecTermVectorsWriter<D> = TermVectorsWriterEnum2<
  TermVectorsWriterEnum2<
    <Lucene90TermVectorsFormat as TermVectorsFormat>::TermVectorsWriter<D>,
    <AssertingTermVectorsFormat as TermVectorsFormat>::TermVectorsWriter<D>,
  >,
  TermVectorsWriterEnum2<
    <CrankyLucene101TermVectorsFormat as TermVectorsFormat>::TermVectorsWriter<D>,
    <CrankyAssertingTermVectorsFormat as TermVectorsFormat>::TermVectorsWriter<D>,
  >,
>;

impl TermVectorsFormat for CodecTermVectorsFormat {
  type TermVectorsReader<I: IndexInput> = CodecTermVectorsReader<I>;

  fn vectors_reader<D1, D2>(
    &self,
    directory: &D1,
    segment_info: &SegmentInfo<D2>,
    field_infos: Arc<FieldInfos>,
    context: &IOContext,
  ) -> Result<Self::TermVectorsReader<D1::IndexInput>>
  where
    D1: Directory,
  {
    match self {
      Self::Lucene90(format) => {
        #[cfg(not(test))]
        {
          format.vectors_reader(directory, segment_info, field_infos, context)
        }
        #[cfg(test)]
        {
          format
            .vectors_reader(directory, segment_info, field_infos, context)
            .map(TermVectorsReaderEnum2::A)
        }
      },
      #[cfg(test)]
      Self::Compressing(format) => format
        .vectors_reader(directory, segment_info, field_infos, context)
        .map(TermVectorsReaderEnum2::A),
      #[cfg(test)]
      Self::Asserting(format) => format
        .vectors_reader(directory, segment_info, field_infos, context)
        .map(TermVectorsReaderEnum2::B),
      #[cfg(test)]
      Self::CrankyLucene101(format) => format
        .vectors_reader(directory, segment_info, field_infos, context)
        .map(TermVectorsReaderEnum2::A),
      #[cfg(test)]
      Self::CrankyAsserting(format) => format
        .vectors_reader(directory, segment_info, field_infos, context)
        .map(TermVectorsReaderEnum2::B),
    }
  }

  type TermVectorsWriter<D: Directory> = CodecTermVectorsWriter<D>;

  fn vectors_writer<D1, D2>(
    &self,
    directory: D1,
    segment_info: &SegmentInfo<D2>,
    context: &IOContext,
  ) -> Result<Self::TermVectorsWriter<D1>>
  where
    D1: Directory,
  {
    match self {
      Self::Lucene90(format) => {
        #[cfg(not(test))]
        {
          format.vectors_writer(directory, segment_info, context)
        }
        #[cfg(test)]
        {
          format
            .vectors_writer(directory, segment_info, context)
            .map(|writer| TermVectorsWriterEnum2::A(TermVectorsWriterEnum2::A(writer)))
        }
      },
      #[cfg(test)]
      Self::Compressing(format) => format
        .vectors_writer(directory, segment_info, context)
        .map(|writer| TermVectorsWriterEnum2::A(TermVectorsWriterEnum2::A(writer))),
      #[cfg(test)]
      Self::Asserting(format) => format
        .vectors_writer(directory, segment_info, context)
        .map(|writer| TermVectorsWriterEnum2::A(TermVectorsWriterEnum2::B(writer))),
      #[cfg(test)]
      Self::CrankyLucene101(format) => format
        .vectors_writer(directory, segment_info, context)
        .map(|writer| TermVectorsWriterEnum2::B(TermVectorsWriterEnum2::A(writer))),
      #[cfg(test)]
      Self::CrankyAsserting(format) => format
        .vectors_writer(directory, segment_info, context)
        .map(|writer| TermVectorsWriterEnum2::B(TermVectorsWriterEnum2::B(writer))),
    }
  }
}

#[cfg(not(test))]
pub type CodecNormsConsumer<O> = <Lucene90NormsFormat as NormsFormat>::NormsConsumer<O>;
#[cfg(test)]
pub type CodecNormsConsumer<O> = NormsConsumerEnum2<
  NormsConsumerEnum2<
    <Lucene90NormsFormat as NormsFormat>::NormsConsumer<O>,
    <AssertingNormsFormat as NormsFormat>::NormsConsumer<O>,
  >,
  NormsConsumerEnum2<
    <CrankyLucene101NormsFormat as NormsFormat>::NormsConsumer<O>,
    <CrankyAssertingNormsFormat as NormsFormat>::NormsConsumer<O>,
  >,
>;

#[cfg(not(test))]
pub type CodecNormsProducer<I> = <Lucene90NormsFormat as NormsFormat>::NormsProducer<I>;
#[cfg(test)]
type Lucene90CodecNormsProducer<I> = <Lucene90NormsFormat as NormsFormat>::NormsProducer<I>;
#[cfg(test)]
type AssertingCodecNormsProducer<I> = <AssertingNormsFormat as NormsFormat>::NormsProducer<I>;

#[cfg(test)]
pub enum CodecNormsProducer<I: IndexInput> {
  Lucene90(Lucene90CodecNormsProducer<I>),
  Asserting(AssertingCodecNormsProducer<I>),
}

#[cfg(test)]
type Lucene90CodecNormNumericDocValues<I> =
  <Lucene90CodecNormsProducer<I> as NormsProducer>::NumericDocValues;
#[cfg(test)]
type AssertingCodecNormNumericDocValues<I> =
  <AssertingCodecNormsProducer<I> as NormsProducer>::NumericDocValues;

#[cfg(test)]
pub enum CodecNormNumericDocValues<I: IndexInput> {
  Lucene90(Lucene90CodecNormNumericDocValues<I>),
  Asserting(AssertingCodecNormNumericDocValues<I>),
}

#[cfg(test)]
impl<I: IndexInput> DocIdSetIterator for CodecNormNumericDocValues<I> {
  fn doc_id(&self) -> i32 {
    match self {
      Self::Lucene90(values) => values.doc_id(),
      Self::Asserting(values) => values.doc_id(),
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    match self {
      Self::Lucene90(values) => values.next_doc(),
      Self::Asserting(values) => values.next_doc(),
    }
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Lucene90(values) => values.advance(target),
      Self::Asserting(values) => values.advance(target),
    }
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Lucene90(values) => values.slow_advance(target),
      Self::Asserting(values) => values.slow_advance(target),
    }
  }

  fn cost(&self) -> Result<i64> {
    match self {
      Self::Lucene90(values) => values.cost(),
      Self::Asserting(values) => values.cost(),
    }
  }
}

#[cfg(test)]
impl<I: IndexInput> DocValuesIterator for CodecNormNumericDocValues<I> {
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    match self {
      Self::Lucene90(values) => values.advance_exact(target),
      Self::Asserting(values) => values.advance_exact(target),
    }
  }
}

#[cfg(test)]
impl<I: IndexInput> NumericDocValues for CodecNormNumericDocValues<I> {
  fn long_value(&mut self) -> Result<i64> {
    match self {
      Self::Lucene90(values) => values.long_value(),
      Self::Asserting(values) => values.long_value(),
    }
  }
}

#[cfg(test)]
impl<I: IndexInput> NormsProducer for CodecNormsProducer<I> {
  type NumericDocValues = CodecNormNumericDocValues<I>;

  fn get_norms(&self, field: &Arc<FieldInfo>) -> Result<Self::NumericDocValues> {
    match self {
      Self::Lucene90(producer) => producer
        .get_norms(field)
        .map(CodecNormNumericDocValues::Lucene90),
      Self::Asserting(producer) => producer
        .get_norms(field)
        .map(CodecNormNumericDocValues::Asserting),
    }
  }

  fn check_integrity(&self) -> Result<()> {
    match self {
      Self::Lucene90(producer) => producer.check_integrity(),
      Self::Asserting(producer) => producer.check_integrity(),
    }
  }

  fn get_merge_instance(&self) -> Result<Option<Self>> {
    match self {
      Self::Lucene90(producer) => producer
        .get_merge_instance()
        .map(|producer| producer.map(Self::Lucene90)),
      Self::Asserting(producer) => producer
        .get_merge_instance()
        .map(|producer| producer.map(Self::Asserting)),
    }
  }
}

#[cfg(test)]
impl<I: IndexInput> CloseableRef for CodecNormsProducer<I> {
  fn close(&self) -> Result<()> {
    match self {
      Self::Lucene90(producer) => producer.close(),
      Self::Asserting(producer) => producer.close(),
    }
  }
}

#[cfg(not(test))]
pub type CodecNormNumericDocValues<I> = <CodecNormsProducer<I> as NormsProducer>::NumericDocValues;

impl NormsFormat for CodecNormsFormat {
  type NormsConsumer<O: IndexOutput> = CodecNormsConsumer<O>;

  fn norms_consumer<D1, D2>(
    &self,
    state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::NormsConsumer<D1::IndexOutput>>
  where
    D1: Directory,
  {
    match self {
      Self::Lucene90(format) => {
        #[cfg(not(test))]
        {
          format.norms_consumer(state, segment_info)
        }
        #[cfg(test)]
        {
          format
            .norms_consumer(state, segment_info)
            .map(|consumer| NormsConsumerEnum2::A(NormsConsumerEnum2::A(consumer)))
        }
      },
      #[cfg(test)]
      Self::Asserting(format) => format
        .norms_consumer(state, segment_info)
        .map(|consumer| NormsConsumerEnum2::A(NormsConsumerEnum2::B(consumer))),
      #[cfg(test)]
      Self::CrankyLucene101(format) => format
        .norms_consumer(state, segment_info)
        .map(|consumer| NormsConsumerEnum2::B(NormsConsumerEnum2::A(consumer))),
      #[cfg(test)]
      Self::CrankyAsserting(format) => format
        .norms_consumer(state, segment_info)
        .map(|consumer| NormsConsumerEnum2::B(NormsConsumerEnum2::B(consumer))),
    }
  }

  type NormsProducer<I: IndexInput> = CodecNormsProducer<I>;

  fn norms_producer<D1, D2>(
    &self,
    state: &SegmentReadState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::NormsProducer<D1::IndexInput>>
  where
    D1: Directory,
  {
    match self {
      Self::Lucene90(format) => {
        #[cfg(not(test))]
        {
          format.norms_producer(state, segment_info)
        }
        #[cfg(test)]
        {
          format
            .norms_producer(state, segment_info)
            .map(CodecNormsProducer::Lucene90)
        }
      },
      #[cfg(test)]
      Self::Asserting(format) => format
        .norms_producer(state, segment_info)
        .map(CodecNormsProducer::Asserting),
      #[cfg(test)]
      Self::CrankyLucene101(format) => format
        .norms_producer(state, segment_info)
        .map(CodecNormsProducer::Lucene90),
      #[cfg(test)]
      Self::CrankyAsserting(format) => format
        .norms_producer(state, segment_info)
        .map(CodecNormsProducer::Asserting),
    }
  }
}

#[cfg(not(test))]
pub type CodecLiveDocsBits = <Lucene90LiveDocsFormat as LiveDocsFormat>::Bits;
#[cfg(test)]
pub enum CodecLiveDocsBits {
  Lucene90(<Lucene90LiveDocsFormat as LiveDocsFormat>::Bits),
  Asserting(<AssertingLiveDocsFormat as LiveDocsFormat>::Bits),
}

#[cfg(test)]
impl HasIdentity for CodecLiveDocsBits {
  fn identity(&self) -> &Identity {
    match self {
      Self::Lucene90(bits) => bits.identity(),
      Self::Asserting(bits) => bits.identity(),
    }
  }
}

#[cfg(test)]
impl Bits for CodecLiveDocsBits {
  fn get(&self, index: usize) -> Result<bool> {
    match self {
      Self::Lucene90(bits) => bits.get(index),
      Self::Asserting(bits) => bits.get(index),
    }
  }

  fn length(&self) -> usize {
    match self {
      Self::Lucene90(bits) => bits.length(),
      Self::Asserting(bits) => bits.length(),
    }
  }

  fn copy_of(&self) -> Result<FixedBitSet> {
    match self {
      Self::Lucene90(bits) => bits.copy_of(),
      Self::Asserting(bits) => bits.copy_of(),
    }
  }

  fn to_string(&self) -> String {
    match self {
      Self::Lucene90(bits) => bits.to_string(),
      Self::Asserting(bits) => bits.to_string(),
    }
  }
}

impl LiveDocsFormat for CodecLiveDocsFormat {
  type Bits = CodecLiveDocsBits;

  fn read_live_docs<D>(
    &self,
    dir: &impl Directory,
    info: &SegmentCommitInfo<D>,
    context: &IOContext,
  ) -> Result<Self::Bits> {
    match self {
      Self::Lucene90(format) => {
        #[cfg(not(test))]
        {
          format.read_live_docs(dir, info, context)
        }
        #[cfg(test)]
        {
          format
            .read_live_docs(dir, info, context)
            .map(CodecLiveDocsBits::Lucene90)
        }
      },
      #[cfg(test)]
      Self::Asserting(format) => format
        .read_live_docs(dir, info, context)
        .map(CodecLiveDocsBits::Asserting),
      #[cfg(test)]
      Self::CrankyLucene101(format) => format
        .read_live_docs(dir, info, context)
        .map(CodecLiveDocsBits::Lucene90),
      #[cfg(test)]
      Self::CrankyAsserting(format) => format
        .read_live_docs(dir, info, context)
        .map(CodecLiveDocsBits::Asserting),
    }
  }

  fn write_live_docs<D>(
    &self,
    bits: &impl Bits,
    dir: &impl Directory,
    info: &SegmentCommitInfo<D>,
    new_del_count: i32,
    context: &IOContext,
  ) -> Result<()> {
    match self {
      Self::Lucene90(format) => format.write_live_docs(bits, dir, info, new_del_count, context),
      #[cfg(test)]
      Self::Asserting(format) => format.write_live_docs(bits, dir, info, new_del_count, context),
      #[cfg(test)]
      Self::CrankyLucene101(format) => {
        format.write_live_docs(bits, dir, info, new_del_count, context)
      },
      #[cfg(test)]
      Self::CrankyAsserting(format) => {
        format.write_live_docs(bits, dir, info, new_del_count, context)
      },
    }
  }

  fn files<D>(&self, info: &SegmentCommitInfo<D>, files: &mut HashSet<String>) -> Result<()> {
    match self {
      Self::Lucene90(format) => format.files(info, files),
      #[cfg(test)]
      Self::Asserting(format) => format.files(info, files),
      #[cfg(test)]
      Self::CrankyLucene101(format) => format.files(info, files),
      #[cfg(test)]
      Self::CrankyAsserting(format) => format.files(info, files),
    }
  }
}

#[cfg(not(test))]
pub type CodecPointsWriter<O> = <Lucene90PointsFormat as PointsFormat>::PointsWriter<O>;
#[cfg(test)]
pub enum CodecPointsWriter<O: IndexOutput> {
  Lucene90(<Lucene90PointsFormat as PointsFormat>::PointsWriter<O>),
  Asserting(<AssertingPointsFormat as PointsFormat>::PointsWriter<O>),
  AssertingNeedsIndexSort(<AssertingNeedsIndexSortPointsFormat as PointsFormat>::PointsWriter<O>),
  CrankyLucene101(<CrankyLucene101PointsFormat as PointsFormat>::PointsWriter<O>),
  CrankyAsserting(<CrankyAssertingPointsFormat as PointsFormat>::PointsWriter<O>),
}

#[cfg(test)]
impl<O: IndexOutput> Closeable for CodecPointsWriter<O> {
  fn close(&mut self) -> Result<()> {
    match self {
      Self::Lucene90(inner) => inner.close(),
      Self::Asserting(inner) => inner.close(),
      Self::AssertingNeedsIndexSort(inner) => inner.close(),
      Self::CrankyLucene101(inner) => inner.close(),
      Self::CrankyAsserting(inner) => inner.close(),
    }
  }
}

#[cfg(test)]
impl<O: IndexOutput> PointsWriter for CodecPointsWriter<O> {
  fn write_field<PR, D1, D2>(
    &mut self,
    field_info: &Arc<FieldInfo>,
    values: &mut PR,
    dir: &D1,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<()>
  where
    PR: PointsReader,
    D1: Directory,
  {
    match self {
      Self::Lucene90(inner) => inner.write_field(field_info, values, dir, segment_info),
      Self::Asserting(inner) => inner.write_field(field_info, values, dir, segment_info),
      Self::AssertingNeedsIndexSort(inner) => {
        inner.write_field(field_info, values, dir, segment_info)
      },
      Self::CrankyLucene101(inner) => inner.write_field(field_info, values, dir, segment_info),
      Self::CrankyAsserting(inner) => inner.write_field(field_info, values, dir, segment_info),
    }
  }

  fn finish(&mut self) -> Result<()> {
    match self {
      Self::Lucene90(inner) => inner.finish(),
      Self::Asserting(inner) => inner.finish(),
      Self::AssertingNeedsIndexSort(inner) => inner.finish(),
      Self::CrankyLucene101(inner) => inner.finish(),
      Self::CrankyAsserting(inner) => inner.finish(),
    }
  }

  fn merge_one_field<D1, D2, CR>(
    &mut self,
    merge_state: &MergeState<D1, CR>,
    field_info: &Arc<FieldInfo>,
    dir: &D2,
  ) -> Result<()>
  where
    D2: Directory,
    CR: CodecReader,
  {
    match self {
      Self::Lucene90(inner) => inner.merge_one_field(merge_state, field_info, dir),
      Self::Asserting(inner) => inner.merge_one_field(merge_state, field_info, dir),
      Self::AssertingNeedsIndexSort(inner) => inner.merge_one_field(merge_state, field_info, dir),
      Self::CrankyLucene101(inner) => inner.merge_one_field(merge_state, field_info, dir),
      Self::CrankyAsserting(inner) => inner.merge_one_field(merge_state, field_info, dir),
    }
  }

  fn merge<D1, D2, CR>(&mut self, merge_state: &MergeState<D1, CR>, dir: &D2) -> Result<()>
  where
    D2: Directory,
    CR: CodecReader,
  {
    match self {
      Self::Lucene90(inner) => inner.merge(merge_state, dir),
      Self::Asserting(inner) => inner.merge(merge_state, dir),
      Self::AssertingNeedsIndexSort(inner) => inner.merge(merge_state, dir),
      Self::CrankyLucene101(inner) => inner.merge(merge_state, dir),
      Self::CrankyAsserting(inner) => inner.merge(merge_state, dir),
    }
  }
}

#[cfg(not(test))]
pub type CodecPointsReader<I> = <Lucene90PointsFormat as PointsFormat>::PointsReader<I>;
#[cfg(test)]
type Lucene90CodecPointsReader<I> = <Lucene90PointsFormat as PointsFormat>::PointsReader<I>;
#[cfg(test)]
type AssertingCodecPointsReader<I> = <AssertingPointsFormat as PointsFormat>::PointsReader<I>;
#[cfg(test)]
type CrankyLucene101CodecPointsReader<I> =
  <CrankyLucene101PointsFormat as PointsFormat>::PointsReader<I>;
#[cfg(test)]
type CrankyAssertingCodecPointsReader<I> =
  <CrankyAssertingPointsFormat as PointsFormat>::PointsReader<I>;

#[cfg(test)]
pub enum CodecPointsReader<I: IndexInput> {
  Lucene90(Lucene90CodecPointsReader<I>),
  Asserting(AssertingCodecPointsReader<I>),
  CrankyLucene101(CrankyLucene101CodecPointsReader<I>),
  CrankyAsserting(CrankyAssertingCodecPointsReader<I>),
}

#[cfg(test)]
type Lucene90CodecPointValues<I> = <Lucene90CodecPointsReader<I> as PointsReader>::PointValuesType;
#[cfg(test)]
type AssertingCodecPointValues<I> =
  <AssertingCodecPointsReader<I> as PointsReader>::PointValuesType;
#[cfg(test)]
type CrankyLucene101CodecPointValues<I> =
  <CrankyLucene101CodecPointsReader<I> as PointsReader>::PointValuesType;
#[cfg(test)]
type CrankyAssertingCodecPointValues<I> =
  <CrankyAssertingCodecPointsReader<I> as PointsReader>::PointValuesType;

#[cfg(test)]
pub enum CodecPointValues<I: IndexInput> {
  Lucene90(Lucene90CodecPointValues<I>),
  Asserting(AssertingCodecPointValues<I>),
  CrankyLucene101(CrankyLucene101CodecPointValues<I>),
  CrankyAsserting(CrankyAssertingCodecPointValues<I>),
}

#[cfg(test)]
impl<I: IndexInput> CloseableRef for CodecPointsReader<I> {
  fn close(&self) -> Result<()> {
    match self {
      Self::Lucene90(reader) => reader.close(),
      Self::Asserting(reader) => reader.close(),
      Self::CrankyLucene101(reader) => reader.close(),
      Self::CrankyAsserting(reader) => reader.close(),
    }
  }
}

#[cfg(test)]
impl<I: IndexInput> PointsReader for CodecPointsReader<I> {
  fn check_integrity(&self) -> Result<()> {
    match self {
      Self::Lucene90(reader) => reader.check_integrity(),
      Self::Asserting(reader) => reader.check_integrity(),
      Self::CrankyLucene101(reader) => reader.check_integrity(),
      Self::CrankyAsserting(reader) => reader.check_integrity(),
    }
  }

  type PointValuesType = CodecPointValues<I>;

  fn get_values(&self, field: &str) -> Result<Option<Self::PointValuesType>> {
    match self {
      Self::Lucene90(reader) => reader
        .get_values(field)
        .map(|values| values.map(CodecPointValues::Lucene90)),
      Self::Asserting(reader) => reader
        .get_values(field)
        .map(|values| values.map(CodecPointValues::Asserting)),
      Self::CrankyLucene101(reader) => reader
        .get_values(field)
        .map(|values| values.map(CodecPointValues::CrankyLucene101)),
      Self::CrankyAsserting(reader) => reader
        .get_values(field)
        .map(|values| values.map(CodecPointValues::CrankyAsserting)),
    }
  }

  fn get_merge_instance(&self) -> Result<Option<Self>> {
    match self {
      Self::Lucene90(reader) => reader
        .get_merge_instance()
        .map(|reader| reader.map(Self::Lucene90)),
      Self::Asserting(reader) => reader
        .get_merge_instance()
        .map(|reader| reader.map(Self::Asserting)),
      Self::CrankyLucene101(reader) => reader
        .get_merge_instance()
        .map(|reader| reader.map(Self::CrankyLucene101)),
      Self::CrankyAsserting(reader) => reader
        .get_merge_instance()
        .map(|reader| reader.map(Self::CrankyAsserting)),
    }
  }
}

#[cfg(test)]
impl<I: IndexInput> PointValues for CodecPointValues<I> {
  fn get_min_packed_value(&self) -> Result<Option<Cow<'_, [u8]>>> {
    match self {
      Self::Lucene90(values) => values.get_min_packed_value(),
      Self::Asserting(values) => values.get_min_packed_value(),
      Self::CrankyLucene101(values) => values.get_min_packed_value(),
      Self::CrankyAsserting(values) => values.get_min_packed_value(),
    }
  }

  fn get_max_packed_value(&self) -> Result<Option<Cow<'_, [u8]>>> {
    match self {
      Self::Lucene90(values) => values.get_max_packed_value(),
      Self::Asserting(values) => values.get_max_packed_value(),
      Self::CrankyLucene101(values) => values.get_max_packed_value(),
      Self::CrankyAsserting(values) => values.get_max_packed_value(),
    }
  }

  fn get_num_dimensions(&self) -> Result<usize> {
    match self {
      Self::Lucene90(values) => values.get_num_dimensions(),
      Self::Asserting(values) => values.get_num_dimensions(),
      Self::CrankyLucene101(values) => values.get_num_dimensions(),
      Self::CrankyAsserting(values) => values.get_num_dimensions(),
    }
  }

  fn get_num_index_dimensions(&self) -> Result<usize> {
    match self {
      Self::Lucene90(values) => values.get_num_index_dimensions(),
      Self::Asserting(values) => values.get_num_index_dimensions(),
      Self::CrankyLucene101(values) => values.get_num_index_dimensions(),
      Self::CrankyAsserting(values) => values.get_num_index_dimensions(),
    }
  }

  fn get_bytes_per_dimension(&self) -> Result<usize> {
    match self {
      Self::Lucene90(values) => values.get_bytes_per_dimension(),
      Self::Asserting(values) => values.get_bytes_per_dimension(),
      Self::CrankyLucene101(values) => values.get_bytes_per_dimension(),
      Self::CrankyAsserting(values) => values.get_bytes_per_dimension(),
    }
  }

  fn size(&self) -> Result<usize> {
    match self {
      Self::Lucene90(values) => values.size(),
      Self::Asserting(values) => values.size(),
      Self::CrankyLucene101(values) => values.size(),
      Self::CrankyAsserting(values) => values.size(),
    }
  }

  fn get_doc_count(&self) -> Result<i32> {
    match self {
      Self::Lucene90(values) => values.get_doc_count(),
      Self::Asserting(values) => values.get_doc_count(),
      Self::CrankyLucene101(values) => values.get_doc_count(),
      Self::CrankyAsserting(values) => values.get_doc_count(),
    }
  }

  type PointTree = PointTreeEnum2<
    PointTreeEnum2<
      <Lucene90CodecPointValues<I> as PointValues>::PointTree,
      <AssertingCodecPointValues<I> as PointValues>::PointTree,
    >,
    PointTreeEnum2<
      <CrankyLucene101CodecPointValues<I> as PointValues>::PointTree,
      <CrankyAssertingCodecPointValues<I> as PointValues>::PointTree,
    >,
  >;
  type MutablePointTree = DummyMutablePointTree;

  fn get_point_tree(&self) -> Result<PointTreeEnum<Self>> {
    match self {
      Self::Lucene90(values) => match values.get_point_tree()? {
        PointTreeEnum::Mutable(_) => dummy_unreachable!(),
        PointTreeEnum::Other(tree) => Ok(PointTreeEnum::Other(PointTreeEnum2::A(
          PointTreeEnum2::A(tree),
        ))),
      },
      Self::Asserting(values) => match values.get_point_tree()? {
        PointTreeEnum::Mutable(_) => dummy_unreachable!(),
        PointTreeEnum::Other(tree) => Ok(PointTreeEnum::Other(PointTreeEnum2::A(
          PointTreeEnum2::B(tree),
        ))),
      },
      Self::CrankyLucene101(values) => match values.get_point_tree()? {
        PointTreeEnum::Mutable(_) => dummy_unreachable!(),
        PointTreeEnum::Other(tree) => Ok(PointTreeEnum::Other(PointTreeEnum2::B(
          PointTreeEnum2::A(tree),
        ))),
      },
      Self::CrankyAsserting(values) => match values.get_point_tree()? {
        PointTreeEnum::Mutable(_) => dummy_unreachable!(),
        PointTreeEnum::Other(tree) => Ok(PointTreeEnum::Other(PointTreeEnum2::B(
          PointTreeEnum2::B(tree),
        ))),
      },
    }
  }
}

impl PointsFormat for CodecPointsFormat {
  type PointsWriter<O: IndexOutput> = CodecPointsWriter<O>;

  fn fields_writer<D1, D2>(
    &self,
    state: &SegmentWriteState<D1>,
    info: &SegmentInfo<D2>,
  ) -> Result<Self::PointsWriter<D1::IndexOutput>>
  where
    D1: Directory,
  {
    match self {
      Self::Lucene90(format) => {
        #[cfg(not(test))]
        {
          format.fields_writer(state, info)
        }
        #[cfg(test)]
        {
          format
            .fields_writer(state, info)
            .map(CodecPointsWriter::Lucene90)
        }
      },
      #[cfg(test)]
      Self::TestLucene90(format) => format
        .fields_writer(state, info)
        .map(CodecPointsWriter::Lucene90),
      #[cfg(test)]
      Self::Asserting(format) => format
        .fields_writer(state, info)
        .map(CodecPointsWriter::Asserting),
      #[cfg(test)]
      Self::AssertingNeedsIndexSort(format) => format
        .fields_writer(state, info)
        .map(CodecPointsWriter::AssertingNeedsIndexSort),
      #[cfg(test)]
      Self::RandomDistance(format) => format
        .fields_writer(state, info)
        .map(CodecPointsWriter::Lucene90),
      #[cfg(test)]
      Self::CrankyLucene101(format) => format
        .fields_writer(state, info)
        .map(CodecPointsWriter::CrankyLucene101),
      #[cfg(test)]
      Self::CrankyAsserting(format) => format
        .fields_writer(state, info)
        .map(CodecPointsWriter::CrankyAsserting),
    }
  }

  type PointsReader<I: IndexInput> = CodecPointsReader<I>;

  fn fields_reader<D1, D2>(
    &self,
    state: &SegmentReadState<D1>,
    info: &SegmentInfo<D2>,
  ) -> Result<Self::PointsReader<D1::IndexInput>>
  where
    D1: Directory,
  {
    match self {
      Self::Lucene90(format) => {
        #[cfg(not(test))]
        {
          format.fields_reader(state, info)
        }
        #[cfg(test)]
        {
          format
            .fields_reader(state, info)
            .map(CodecPointsReader::Lucene90)
        }
      },
      #[cfg(test)]
      Self::TestLucene90(format) => format
        .fields_reader(state, info)
        .map(CodecPointsReader::Lucene90),
      #[cfg(test)]
      Self::Asserting(format) => format
        .fields_reader(state, info)
        .map(CodecPointsReader::Asserting),
      #[cfg(test)]
      Self::AssertingNeedsIndexSort(format) => format
        .fields_reader(state, info)
        .map(CodecPointsReader::Lucene90),
      #[cfg(test)]
      Self::RandomDistance(format) => format
        .fields_reader(state, info)
        .map(CodecPointsReader::Lucene90),
      #[cfg(test)]
      Self::CrankyLucene101(format) => format
        .fields_reader(state, info)
        .map(CodecPointsReader::CrankyLucene101),
      #[cfg(test)]
      Self::CrankyAsserting(format) => format
        .fields_reader(state, info)
        .map(CodecPointsReader::CrankyAsserting),
    }
  }
}

#[cfg(not(test))]
pub type CodecKnnVectorsWriter<O> =
  <Lucene101CodecKnnVectorsFormat as KnnVectorsFormat>::KnnVectorsWriter<O>;
#[cfg(test)]
pub enum CodecKnnVectorsWriter<O: IndexOutput> {
  Lucene101(<Lucene101CodecKnnVectorsFormat as KnnVectorsFormat>::KnnVectorsWriter<O>),
  Asserting(<AssertingKnnVectorsFormat as KnnVectorsFormat>::KnnVectorsWriter<O>),
}

#[cfg(test)]
impl<O: IndexOutput> Closeable for CodecKnnVectorsWriter<O> {
  fn close(&mut self) -> Result<()> {
    match self {
      Self::Lucene101(inner) => inner.close(),
      Self::Asserting(inner) => inner.close(),
    }
  }
}

#[cfg(test)]
impl<O: IndexOutput> Accountable for CodecKnnVectorsWriter<O> {
  fn ram_bytes_used(&self) -> Result<i64> {
    match self {
      Self::Lucene101(inner) => inner.ram_bytes_used(),
      Self::Asserting(inner) => inner.ram_bytes_used(),
    }
  }
}

#[cfg(test)]
impl<O: IndexOutput> KnnVectorsWriter<O> for CodecKnnVectorsWriter<O> {
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
      Self::Lucene101(inner) => inner.add_field(write_state, segment_info, field_info),
      Self::Asserting(inner) => inner.add_field(write_state, segment_info, field_info),
    }
  }

  fn flush<DM>(&mut self, max_doc: i32, sort_map: Option<&DM>) -> Result<()>
  where
    DM: DocMap,
  {
    match self {
      Self::Lucene101(inner) => inner.flush(max_doc, sort_map),
      Self::Asserting(inner) => inner.flush(max_doc, sort_map),
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
      Self::Lucene101(inner) => inner.merge_one_field(field_info, merge_state, segment_write_state),
      Self::Asserting(inner) => inner.merge_one_field(field_info, merge_state, segment_write_state),
    }
  }

  fn finish(&mut self) -> Result<()> {
    match self {
      Self::Lucene101(inner) => inner.finish(),
      Self::Asserting(inner) => inner.finish(),
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
      Self::Lucene101(inner) => inner.merge(merge_state, segment_write_state),
      Self::Asserting(inner) => inner.merge(merge_state, segment_write_state),
    }
  }

  fn finish_merge<D, CR>(&self, merge_state: &MergeState<'_, D, CR>) -> Result<()>
  where
    CR: CodecReader,
  {
    match self {
      Self::Lucene101(inner) => inner.finish_merge(merge_state),
      Self::Asserting(inner) => inner.finish_merge(merge_state),
    }
  }

  fn add_value(
    &mut self,
    doc_id: i32,
    vector_value: &VectorValueEnum,
    field_vectors_writers_idx: usize,
  ) -> Result<()> {
    match self {
      Self::Lucene101(inner) => inner.add_value(doc_id, vector_value, field_vectors_writers_idx),
      Self::Asserting(inner) => inner.add_value(doc_id, vector_value, field_vectors_writers_idx),
    }
  }
}

#[cfg(not(test))]
pub type CodecKnnVectorsReader<I> =
  <Lucene101CodecKnnVectorsFormat as KnnVectorsFormat>::KnnVectorsReader<I>;
#[cfg(test)]
pub(crate) enum CodecKnnVectorsReaderInner<I: IndexInput> {
  Lucene101(<Lucene101CodecKnnVectorsFormat as KnnVectorsFormat>::KnnVectorsReader<I>),
  Asserting(<AssertingKnnVectorsFormat as KnnVectorsFormat>::KnnVectorsReader<I>),
}

#[cfg(test)]
impl<I: IndexInput> CloseableRef for CodecKnnVectorsReaderInner<I> {
  fn close(&self) -> Result<()> {
    match self {
      Self::Lucene101(reader) => reader.close(),
      Self::Asserting(reader) => reader.close(),
    }
  }
}

#[cfg(test)]
impl<I: IndexInput> crate::core::codecs::hnsw::hnsw_graph_provider::HnswGraphProvider
  for CodecKnnVectorsReaderInner<I>
{
  type HnswGraph = <KnnVectorsFormatsReader<I> as crate::core::codecs::hnsw::hnsw_graph_provider::HnswGraphProvider>::HnswGraph;

  fn is_hnsw_graph_provider(&self, field: &str) -> bool {
    match self {
      Self::Lucene101(reader) => reader.is_hnsw_graph_provider(field),
      Self::Asserting(reader) => reader.is_hnsw_graph_provider(field),
    }
  }

  fn get_graph(&self, field: &str) -> Result<Self::HnswGraph> {
    match self {
      Self::Lucene101(reader) => reader.get_graph(field),
      Self::Asserting(reader) => reader.get_graph(field),
    }
  }
}

#[cfg(test)]
impl<I: IndexInput> KnnVectorsReader for CodecKnnVectorsReaderInner<I> {
  fn check_integrity(&self) -> Result<()> {
    match self {
      Self::Lucene101(reader) => reader.check_integrity(),
      Self::Asserting(reader) => reader.check_integrity(),
    }
  }

  type FloatVectorValues = <KnnVectorsFormatsReader<I> as KnnVectorsReader>::FloatVectorValues;

  fn get_float_vector_values(&self, field: &str) -> Result<Self::FloatVectorValues> {
    match self {
      Self::Lucene101(reader) => reader.get_float_vector_values(field),
      Self::Asserting(reader) => reader.get_float_vector_values(field),
    }
  }

  type ByteVectorValues = <KnnVectorsFormatsReader<I> as KnnVectorsReader>::ByteVectorValues;

  fn get_byte_vector_values(&self, field: &str) -> Result<Self::ByteVectorValues> {
    match self {
      Self::Lucene101(reader) => reader.get_byte_vector_values(field),
      Self::Asserting(reader) => reader.get_byte_vector_values(field),
    }
  }

  fn get_quantization_state(
    &self,
    field: &str,
  ) -> Result<Option<crate::core::util::quantization::scalar_quantizer::ScalarQuantizer>> {
    match self {
      Self::Lucene101(reader) => reader.get_quantization_state(field),
      Self::Asserting(reader) => reader.get_quantization_state(field),
    }
  }

  fn is_flat_vectors_reader(&self, field: &str) -> bool {
    match self {
      Self::Lucene101(reader) => reader.is_flat_vectors_reader(field),
      Self::Asserting(reader) => reader.is_flat_vectors_reader(field),
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
    K: crate::core::search::knn_collector::KnnCollector,
  {
    match self {
      Self::Lucene101(reader) => reader.search_f32(field, target, knn_collector, accept_docs),
      Self::Asserting(reader) => reader.search_f32(field, target, knn_collector, accept_docs),
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
    K: crate::core::search::knn_collector::KnnCollector,
  {
    match self {
      Self::Lucene101(reader) => reader.search_u8(field, target, knn_collector, accept_docs),
      Self::Asserting(reader) => reader.search_u8(field, target, knn_collector, accept_docs),
    }
  }

  fn get_merge_instance(&self) -> Result<Option<Self>> {
    match self {
      Self::Lucene101(reader) => reader
        .get_merge_instance()
        .map(|reader| reader.map(Self::Lucene101)),
      Self::Asserting(reader) => reader
        .get_merge_instance()
        .map(|reader| reader.map(Self::Asserting)),
    }
  }

  fn finish_merge(&self) -> Result<()> {
    match self {
      Self::Lucene101(reader) => reader.finish_merge(),
      Self::Asserting(reader) => reader.finish_merge(),
    }
  }
}

#[cfg(test)]
type CodecFloatVectorValuesInner<I> =
  <KnnVectorsFormatsReader<I> as KnnVectorsReader>::FloatVectorValues;

#[cfg(test)]
type CodecFloatDocIndexIteratorInner<I> =
  <CodecFloatVectorValuesInner<I> as KnnVectorValues>::DocIndexIterator;

#[cfg(test)]
pub struct CodecFloatDocIndexIterator<I>(CodecFloatDocIndexIteratorInner<I>)
where
  I: IndexInput;

#[cfg(test)]
impl<I: IndexInput> DocIdSetIterator for CodecFloatDocIndexIterator<I> {
  fn doc_id(&self) -> i32 {
    self.0.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.0.next_doc()
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.0.advance(target)
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    self.0.slow_advance(target)
  }

  fn cost(&self) -> Result<i64> {
    self.0.cost()
  }
}

#[cfg(test)]
impl<I: IndexInput> DocIndexIterator for CodecFloatDocIndexIterator<I> {
  fn index(&self) -> Result<i32> {
    self.0.index()
  }
}

#[cfg(test)]
type CodecFloatVectorScorerInner<I> =
  <CodecFloatVectorValuesInner<I> as FloatVectorValues>::VectorScorer;

#[cfg(test)]
type CodecFloatVectorScorerIteratorRefInner<'a, I> =
  <CodecFloatVectorScorerInner<I> as VectorScorer>::DocIdSetIteratorRef<'a>;

#[cfg(test)]
pub struct CodecFloatVectorScorerIteratorRef<'a, I>(CodecFloatVectorScorerIteratorRefInner<'a, I>)
where
  I: IndexInput + 'a;

#[cfg(test)]
impl<'a, I: IndexInput + 'a> DocIdSetIterator for CodecFloatVectorScorerIteratorRef<'a, I> {
  fn doc_id(&self) -> i32 {
    self.0.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.0.next_doc()
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.0.advance(target)
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    self.0.slow_advance(target)
  }

  fn cost(&self) -> Result<i64> {
    self.0.cost()
  }
}

#[cfg(test)]
type CodecFloatVectorScorerIteratorMutInner<'a, I> =
  <CodecFloatVectorScorerInner<I> as VectorScorer>::DocIdSetIteratorMut<'a>;

#[cfg(test)]
pub struct CodecFloatVectorScorerIteratorMut<'a, I>(CodecFloatVectorScorerIteratorMutInner<'a, I>)
where
  I: IndexInput + 'a;

#[cfg(test)]
impl<'a, I: IndexInput + 'a> DocIdSetIterator for CodecFloatVectorScorerIteratorMut<'a, I> {
  fn doc_id(&self) -> i32 {
    self.0.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.0.next_doc()
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.0.advance(target)
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    self.0.slow_advance(target)
  }

  fn cost(&self) -> Result<i64> {
    self.0.cost()
  }
}

#[cfg(test)]
pub struct CodecFloatVectorScorer<I>(CodecFloatVectorScorerInner<I>)
where
  I: IndexInput;

#[cfg(test)]
impl<I: IndexInput> VectorScorer for CodecFloatVectorScorer<I> {
  fn score(&self) -> Result<f32> {
    self.0.score()
  }

  type DocIdSetIteratorRef<'a>
    = CodecFloatVectorScorerIteratorRef<'a, I>
  where
    Self: 'a;

  fn iterator(&self) -> Self::DocIdSetIteratorRef<'_> {
    CodecFloatVectorScorerIteratorRef(self.0.iterator())
  }

  type DocIdSetIteratorMut<'a>
    = CodecFloatVectorScorerIteratorMut<'a, I>
  where
    Self: 'a;

  fn iterator_mut(&mut self) -> Self::DocIdSetIteratorMut<'_> {
    CodecFloatVectorScorerIteratorMut(self.0.iterator_mut())
  }
}

#[cfg(test)]
pub struct CodecFloatVectorValues<I>(CodecFloatVectorValuesInner<I>)
where
  I: IndexInput;

#[cfg(test)]
impl<I> KnnVectorValues for CodecFloatVectorValues<I>
where
  I: IndexInput,
{
  fn dimension(&self) -> usize {
    self.0.dimension()
  }

  fn size(&self) -> usize {
    self.0.size()
  }

  fn ord_to_doc(&self, ord: usize) -> Result<usize> {
    self.0.ord_to_doc(ord)
  }

  type KnnVectorValues = <CodecFloatVectorValuesInner<I> as KnnVectorValues>::KnnVectorValues;

  fn copy(&self) -> Result<Self::KnnVectorValues> {
    self.0.copy()
  }

  fn get_vector_byte_length(&self) -> usize {
    self.0.get_vector_byte_length()
  }

  fn get_encoding(&self) -> crate::core::index::vector_encoding::VectorEncoding {
    KnnVectorValues::get_encoding(&self.0)
  }

  type Bits<'a, B>
    = <CodecFloatVectorValuesInner<I> as KnnVectorValues>::Bits<'a, B>
  where
    B: Bits,
    Self: 'a;

  fn get_accept_ords<'a, B>(&'a self, accept_docs: Option<B>) -> Option<Self::Bits<'a, B>>
  where
    B: Bits,
  {
    self.0.get_accept_ords(accept_docs)
  }

  type DocIndexIterator = CodecFloatDocIndexIterator<I>;

  fn iterator(&self) -> Result<Self::DocIndexIterator> {
    self.0.iterator().map(CodecFloatDocIndexIterator)
  }
}

#[cfg(test)]
impl<I> FloatVectorValues for CodecFloatVectorValues<I>
where
  I: IndexInput,
{
  fn vector_value(
    &self,
    ord: usize,
  ) -> Result<std::borrow::Cow<'_, crate::core::codecs::knn_field_vectors_writer::VectorValueEnum>>
  {
    self.0.vector_value(ord)
  }

  type FloatVectorValues = <CodecFloatVectorValuesInner<I> as FloatVectorValues>::FloatVectorValues;

  fn float_copy(&self) -> Result<Option<Self::FloatVectorValues>> {
    self.0.float_copy()
  }

  type VectorScorer = CodecFloatVectorScorer<I>;

  fn scorer(&self, target: Vec<f32>) -> Result<Option<Self::VectorScorer>> {
    self
      .0
      .scorer(target)
      .map(|scorer| scorer.map(CodecFloatVectorScorer))
  }

  fn get_encoding(&self) -> crate::core::index::vector_encoding::VectorEncoding {
    FloatVectorValues::get_encoding(&self.0)
  }

  fn get_vectors_mut(
    &mut self,
  ) -> Result<&mut Vec<crate::core::codecs::knn_field_vectors_writer::VectorValueEnum>> {
    self.0.get_vectors_mut()
  }

  fn get_vectors(
    &self,
  ) -> Result<&[crate::core::codecs::knn_field_vectors_writer::VectorValueEnum]> {
    self.0.get_vectors()
  }

  fn get_vectors_capacity(&self) -> Result<usize> {
    self.0.get_vectors_capacity()
  }
}

#[cfg(test)]
type CodecByteVectorValuesInner<I> =
  <KnnVectorsFormatsReader<I> as KnnVectorsReader>::ByteVectorValues;

#[cfg(test)]
type CodecByteDocIndexIteratorInner<I> =
  <CodecByteVectorValuesInner<I> as KnnVectorValues>::DocIndexIterator;

#[cfg(test)]
pub struct CodecByteDocIndexIterator<I>(CodecByteDocIndexIteratorInner<I>)
where
  I: IndexInput;

#[cfg(test)]
impl<I: IndexInput> DocIdSetIterator for CodecByteDocIndexIterator<I> {
  fn doc_id(&self) -> i32 {
    self.0.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.0.next_doc()
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.0.advance(target)
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    self.0.slow_advance(target)
  }

  fn cost(&self) -> Result<i64> {
    self.0.cost()
  }
}

#[cfg(test)]
impl<I: IndexInput> DocIndexIterator for CodecByteDocIndexIterator<I> {
  fn index(&self) -> Result<i32> {
    self.0.index()
  }
}

#[cfg(test)]
type CodecByteVectorScorerInner<I> =
  <CodecByteVectorValuesInner<I> as ByteVectorValues>::VectorScorer;

#[cfg(test)]
type CodecByteVectorScorerIteratorRefInner<'a, I> =
  <CodecByteVectorScorerInner<I> as VectorScorer>::DocIdSetIteratorRef<'a>;

#[cfg(test)]
pub struct CodecByteVectorScorerIteratorRef<'a, I>(CodecByteVectorScorerIteratorRefInner<'a, I>)
where
  I: IndexInput + 'a;

#[cfg(test)]
impl<'a, I: IndexInput + 'a> DocIdSetIterator for CodecByteVectorScorerIteratorRef<'a, I> {
  fn doc_id(&self) -> i32 {
    self.0.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.0.next_doc()
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.0.advance(target)
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    self.0.slow_advance(target)
  }

  fn cost(&self) -> Result<i64> {
    self.0.cost()
  }
}

#[cfg(test)]
type CodecByteVectorScorerIteratorMutInner<'a, I> =
  <CodecByteVectorScorerInner<I> as VectorScorer>::DocIdSetIteratorMut<'a>;

#[cfg(test)]
pub struct CodecByteVectorScorerIteratorMut<'a, I>(CodecByteVectorScorerIteratorMutInner<'a, I>)
where
  I: IndexInput + 'a;

#[cfg(test)]
impl<'a, I: IndexInput + 'a> DocIdSetIterator for CodecByteVectorScorerIteratorMut<'a, I> {
  fn doc_id(&self) -> i32 {
    self.0.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.0.next_doc()
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.0.advance(target)
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    self.0.slow_advance(target)
  }

  fn cost(&self) -> Result<i64> {
    self.0.cost()
  }
}

#[cfg(test)]
pub struct CodecByteVectorScorer<I>(CodecByteVectorScorerInner<I>)
where
  I: IndexInput;

#[cfg(test)]
impl<I: IndexInput> VectorScorer for CodecByteVectorScorer<I> {
  fn score(&self) -> Result<f32> {
    self.0.score()
  }

  type DocIdSetIteratorRef<'a>
    = CodecByteVectorScorerIteratorRef<'a, I>
  where
    Self: 'a;

  fn iterator(&self) -> Self::DocIdSetIteratorRef<'_> {
    CodecByteVectorScorerIteratorRef(self.0.iterator())
  }

  type DocIdSetIteratorMut<'a>
    = CodecByteVectorScorerIteratorMut<'a, I>
  where
    Self: 'a;

  fn iterator_mut(&mut self) -> Self::DocIdSetIteratorMut<'_> {
    CodecByteVectorScorerIteratorMut(self.0.iterator_mut())
  }
}

#[cfg(test)]
pub struct CodecByteVectorValues<I>(CodecByteVectorValuesInner<I>)
where
  I: IndexInput;

#[cfg(test)]
impl<I> KnnVectorValues for CodecByteVectorValues<I>
where
  I: IndexInput,
{
  fn dimension(&self) -> usize {
    self.0.dimension()
  }

  fn size(&self) -> usize {
    self.0.size()
  }

  fn ord_to_doc(&self, ord: usize) -> Result<usize> {
    self.0.ord_to_doc(ord)
  }

  type KnnVectorValues = <CodecByteVectorValuesInner<I> as KnnVectorValues>::KnnVectorValues;

  fn copy(&self) -> Result<Self::KnnVectorValues> {
    self.0.copy()
  }

  fn get_vector_byte_length(&self) -> usize {
    self.0.get_vector_byte_length()
  }

  fn get_encoding(&self) -> crate::core::index::vector_encoding::VectorEncoding {
    KnnVectorValues::get_encoding(&self.0)
  }

  type Bits<'a, B>
    = <CodecByteVectorValuesInner<I> as KnnVectorValues>::Bits<'a, B>
  where
    B: Bits,
    Self: 'a;

  fn get_accept_ords<'a, B>(&'a self, accept_docs: Option<B>) -> Option<Self::Bits<'a, B>>
  where
    B: Bits,
  {
    self.0.get_accept_ords(accept_docs)
  }

  type DocIndexIterator = CodecByteDocIndexIterator<I>;

  fn iterator(&self) -> Result<Self::DocIndexIterator> {
    self.0.iterator().map(CodecByteDocIndexIterator)
  }
}

#[cfg(test)]
impl<I> ByteVectorValues for CodecByteVectorValues<I>
where
  I: IndexInput,
{
  fn vector_value(
    &self,
    ord: usize,
  ) -> Result<std::borrow::Cow<'_, crate::core::codecs::knn_field_vectors_writer::VectorValueEnum>>
  {
    self.0.vector_value(ord)
  }

  type ByteVectorValues = <CodecByteVectorValuesInner<I> as ByteVectorValues>::ByteVectorValues;

  fn byte_copy(&self) -> Result<Option<Self::ByteVectorValues>> {
    self.0.byte_copy()
  }

  type VectorScorer = CodecByteVectorScorer<I>;

  fn scorer(&self, target: Vec<u8>) -> Result<Option<Self::VectorScorer>> {
    self
      .0
      .scorer(target)
      .map(|scorer| scorer.map(CodecByteVectorScorer))
  }

  fn get_encoding(&self) -> crate::core::index::vector_encoding::VectorEncoding {
    ByteVectorValues::get_encoding(&self.0)
  }

  fn get_vectors_mut(
    &mut self,
  ) -> Result<&mut Vec<crate::core::codecs::knn_field_vectors_writer::VectorValueEnum>> {
    self.0.get_vectors_mut()
  }

  fn get_vectors(
    &self,
  ) -> Result<&[crate::core::codecs::knn_field_vectors_writer::VectorValueEnum]> {
    self.0.get_vectors()
  }

  fn get_vectors_capacity(&self) -> Result<usize> {
    self.0.get_vectors_capacity()
  }
}

#[cfg(test)]
type CodecHnswGraphInner<I> = <KnnVectorsFormatsReader<I> as crate::core::codecs::hnsw::hnsw_graph_provider::HnswGraphProvider>::HnswGraph;

#[cfg(test)]
type CodecHnswGraphNodesIteratorInner<I> = <CodecHnswGraphInner<I> as HnswGraph>::NodeIterator;

#[cfg(test)]
pub struct CodecHnswGraphNodesIterator<I>(CodecHnswGraphNodesIteratorInner<I>)
where
  I: IndexInput;

#[cfg(test)]
impl<I: IndexInput> Iterator for CodecHnswGraphNodesIterator<I> {
  type Item = usize;

  fn next(&mut self) -> Option<Self::Item> {
    self.0.next()
  }
}

#[cfg(test)]
impl<I: IndexInput> NodesIterator for CodecHnswGraphNodesIterator<I> {
  fn size(&self) -> usize {
    self.0.size()
  }

  fn consume(&mut self, dest: &mut [usize]) -> Result<usize> {
    self.0.consume(dest)
  }

  fn has_next(&self) -> bool {
    self.0.has_next()
  }
}

#[cfg(test)]
pub struct CodecHnswGraph<I>(CodecHnswGraphInner<I>)
where
  I: IndexInput;

#[cfg(test)]
impl<I: IndexInput> HnswGraph for CodecHnswGraph<I> {
  fn seek(&mut self, level: usize, target: usize) -> Result<()> {
    self.0.seek(level, target)
  }

  fn size(&self) -> usize {
    self.0.size()
  }

  fn max_node_id(&self) -> Option<usize> {
    self.0.max_node_id()
  }

  fn next_neighbor(&mut self) -> Result<usize> {
    self.0.next_neighbor()
  }

  fn num_levels(&self) -> Result<usize> {
    self.0.num_levels()
  }

  fn entry_node(&self) -> Result<Option<usize>> {
    self.0.entry_node()
  }

  type NodeIterator = CodecHnswGraphNodesIterator<I>;

  fn get_nodes_on_level(&mut self, level: usize) -> Result<Self::NodeIterator> {
    self
      .0
      .get_nodes_on_level(level)
      .map(CodecHnswGraphNodesIterator)
  }

  fn get_neighbors_mut(&mut self, level: usize, node: usize) -> Result<&mut NeighborArray> {
    self.0.get_neighbors_mut(level, node)
  }

  fn get_neighbors(&self, level: usize, node: usize) -> Result<&NeighborArray> {
    self.0.get_neighbors(level, node)
  }
}

#[cfg(test)]
pub struct CodecKnnVectorsReader<I>(CodecKnnVectorsReaderInner<I>)
where
  I: IndexInput;

#[cfg(test)]
impl<I> CodecKnnVectorsReader<I>
where
  I: IndexInput,
{
  pub(crate) fn as_inner(&self) -> &CodecKnnVectorsReaderInner<I> {
    &self.0
  }
}

#[cfg(test)]
impl<I> crate::core::util::close::CloseableRef for CodecKnnVectorsReader<I>
where
  I: IndexInput,
{
  fn close(&self) -> Result<()> {
    self.0.close()
  }
}

#[cfg(test)]
impl<I> crate::core::codecs::hnsw::hnsw_graph_provider::HnswGraphProvider
  for CodecKnnVectorsReader<I>
where
  I: IndexInput,
{
  type HnswGraph = CodecHnswGraph<I>;

  fn is_hnsw_graph_provider(&self, field: &str) -> bool {
    self.0.is_hnsw_graph_provider(field)
  }

  fn get_graph(&self, field: &str) -> Result<Self::HnswGraph> {
    self.0.get_graph(field).map(CodecHnswGraph)
  }
}

#[cfg(test)]
impl<I> KnnVectorsReader for CodecKnnVectorsReader<I>
where
  I: IndexInput,
{
  fn check_integrity(&self) -> Result<()> {
    self.0.check_integrity()
  }

  type FloatVectorValues = CodecFloatVectorValues<I>;

  fn get_float_vector_values(&self, field: &str) -> Result<Self::FloatVectorValues> {
    self
      .0
      .get_float_vector_values(field)
      .map(CodecFloatVectorValues)
  }

  type ByteVectorValues = CodecByteVectorValues<I>;

  fn get_byte_vector_values(&self, field: &str) -> Result<Self::ByteVectorValues> {
    self
      .0
      .get_byte_vector_values(field)
      .map(CodecByteVectorValues)
  }

  fn get_quantization_state(
    &self,
    field: &str,
  ) -> Result<Option<crate::core::util::quantization::scalar_quantizer::ScalarQuantizer>> {
    self.0.get_quantization_state(field)
  }

  fn is_flat_vectors_reader(&self, field: &str) -> bool {
    self.0.is_flat_vectors_reader(field)
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
    K: crate::core::search::knn_collector::KnnCollector,
  {
    self.0.search_f32(field, target, knn_collector, accept_docs)
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
    K: crate::core::search::knn_collector::KnnCollector,
  {
    self.0.search_u8(field, target, knn_collector, accept_docs)
  }

  fn get_merge_instance(&self) -> Result<Option<Self>> {
    self
      .0
      .get_merge_instance()
      .map(|reader| reader.map(CodecKnnVectorsReader))
  }

  fn finish_merge(&self) -> Result<()> {
    self.0.finish_merge()
  }
}

impl Display for CodecKnnVectorsFormat {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Lucene101(format) => Display::fmt(format, f),
      #[cfg(test)]
      Self::Asserting(format) => Display::fmt(format, f),
    }
  }
}

impl HasIdentity for CodecKnnVectorsFormat {
  fn identity(&self) -> &Identity {
    match self {
      Self::Lucene101(format) => format.identity(),
      #[cfg(test)]
      Self::Asserting(format) => format.identity(),
    }
  }
}

impl KnnVectorsFormat for CodecKnnVectorsFormat {
  fn get_name(&self) -> &str {
    match self {
      Self::Lucene101(format) => format.get_name(),
      #[cfg(test)]
      Self::Asserting(format) => format.get_name(),
    }
  }

  type KnnVectorsWriter<O: IndexOutput> = CodecKnnVectorsWriter<O>;

  fn fields_writer<D1, D2>(
    &self,
    state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::KnnVectorsWriter<D1::IndexOutput>>
  where
    D1: Directory,
  {
    match self {
      Self::Lucene101(format) => {
        #[cfg(not(test))]
        {
          format.fields_writer(state, segment_info)
        }
        #[cfg(test)]
        {
          format
            .fields_writer(state, segment_info)
            .map(CodecKnnVectorsWriter::Lucene101)
        }
      },
      #[cfg(test)]
      Self::Asserting(format) => format
        .fields_writer(state, segment_info)
        .map(CodecKnnVectorsWriter::Asserting),
    }
  }

  type KnnVectorsReader<I: IndexInput> = CodecKnnVectorsReader<I>;

  fn fields_reader<D1, D2>(
    &self,
    state: &SegmentReadState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::KnnVectorsReader<D1::IndexInput>>
  where
    D1: Directory,
  {
    match self {
      Self::Lucene101(format) => {
        #[cfg(not(test))]
        {
          format.fields_reader(state, segment_info)
        }
        #[cfg(test)]
        {
          format
            .fields_reader(state, segment_info)
            .map(|reader| CodecKnnVectorsReader(CodecKnnVectorsReaderInner::Lucene101(reader)))
        }
      },
      #[cfg(test)]
      Self::Asserting(format) => format
        .fields_reader(state, segment_info)
        .map(|reader| CodecKnnVectorsReader(CodecKnnVectorsReaderInner::Asserting(reader))),
    }
  }

  fn get_max_dimensions(&self, field_name: &str) -> Result<usize> {
    match self {
      Self::Lucene101(format) => format.get_max_dimensions(field_name),
      #[cfg(test)]
      Self::Asserting(format) => format.get_max_dimensions(field_name),
    }
  }

  fn for_name(name: &str) -> Result<Arc<Self>> {
    Err(LuceneError::illegal_argument(format!(
      "Could not load vectors format named \"{name}\""
    )))
  }
}
