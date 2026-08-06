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
use crate::core::codecs::codec_formats::{
  CodecCompoundFormat, CodecFieldInfosFormat, CodecSegmentInfoFormat,
};
use crate::core::codecs::codec_formats::{
  CodecDocValuesFormat, CodecKnnVectorsFormat, CodecLiveDocsFormat, CodecNormsFormat,
  CodecPointsFormat, CodecPostingsFormat, CodecStoredFieldsFormat, CodecTermVectorsFormat,
};
use crate::core::codecs::compound_format::CompoundFormat;
use crate::core::codecs::doc_values_format::DocValuesFormat;
use crate::core::codecs::field_infos_format::FieldInfosFormat;
use crate::core::codecs::knn_vectors_format::KnnVectorsFormat;
use crate::core::codecs::live_docs_format::LiveDocsFormat;
use crate::core::codecs::lucene101_codec::Lucene101Codec;
use crate::core::codecs::norms_format::NormsFormat;
use crate::core::codecs::points_format::PointsFormat;
use crate::core::codecs::postings_format::PostingsFormat;
use crate::core::codecs::segment_info_format::SegmentInfoFormat;
use crate::core::codecs::stored_fields_format::StoredFieldsFormat;
use crate::core::codecs::term_vectors_format::TermVectorsFormat;
use crate::core::util::error::lucene_error::{LuceneError, Result};
#[cfg(test)]
use crate::test_framework::core::codecs::asserting_codec::AssertingCodec;
#[cfg(test)]
use crate::test_framework::core::codecs::compressing::compressing_codec::CompressingCodec;
#[cfg(test)]
use crate::test_framework::core::codecs::compressing::dummy::dummy_compressing_codec::DummyCompressingCodec;
#[cfg(test)]
use crate::test_framework::core::codecs::cranky::cranky_codec::CrankyCodec;
#[cfg(test)]
use crate::test_framework::core::codecs::lucene90::test_lucene90_points_format::TestLucene90PointsFormatCodec;
#[cfg(test)]
use crate::test_framework::core::codecs::test_minimal_codec::{MinimalCodec, MinimalCompoundCodec};
#[cfg(test)]
use crate::test_framework::core::geo::random_distance_codec::RandomDistanceCodec;
#[cfg(test)]
use crate::test_framework::core::index::base_postings_format_test_case::InvertedWriteCodec;
#[cfg(test)]
use crate::test_framework::core::index::test_add_indexes::UnRegisteredCodec;
#[cfg(test)]
use crate::test_framework::core::index::test_index_sorting::AssertingNeedsIndexSortCodec;
#[cfg(test)]
use crate::test_framework::core::index::test_index_writer_force_merge::MergePerFieldCodec;
#[cfg(not(test))]
use parking_lot::RwLock;
#[cfg(test)]
use std::cell::RefCell;
use std::fmt::{Display, Formatter};
#[cfg(not(test))]
use std::sync::LazyLock;

pub trait Codec: Display {
  type PostingsFormat: PostingsFormat;
  type DocValuesFormat: DocValuesFormat;
  type StoredFieldsFormat: StoredFieldsFormat;
  type TermVectorsFormat: TermVectorsFormat;
  type FieldInfosFormat: FieldInfosFormat;
  type SegmentInfoFormat: SegmentInfoFormat;
  type NormsFormat: NormsFormat;
  type LiveDocsFormat: LiveDocsFormat;
  type CompoundFormat: CompoundFormat;
  type PointsFormat: PointsFormat;
  type KnnVectorsFormat: KnnVectorsFormat;
  // type KnnVectorsFormat;
  /// Encodes/decodes postings
  fn postings_format(&self) -> Self::PostingsFormat;
  /// Encodes/decodes docvalues
  fn doc_values_format(&self) -> Self::DocValuesFormat;
  //
  /// Encodes/decodes stored fields
  fn stored_fields_format(&self) -> Self::StoredFieldsFormat;
  //
  /// Encodes/decodes term vectors
  fn term_vectors_format(&self) -> Self::TermVectorsFormat;

  /// Encodes/decodes field infos file
  fn field_infos_format(&self) -> Self::FieldInfosFormat;

  /// Encodes/decodes segment info file
  fn segment_info_format(&self) -> Self::SegmentInfoFormat;

  // /// Encodes/decodes document normalization values
  fn norms_format(&self) -> Self::NormsFormat;

  /// Encodes/decodes live docs
  fn live_docs_format(&self) -> Self::LiveDocsFormat;

  /// Encodes/decodes compound files
  fn compound_format(&self) -> Self::CompoundFormat;

  /// Encodes/decodes points index
  fn points_format(&self) -> Self::PointsFormat;

  /// Encodes/decodes numeric vector fields
  fn knn_vectors_format(&self) -> Result<Self::KnnVectorsFormat>;

  fn get_name(&self) -> &str;
}

#[derive(Clone)]
pub enum Codecs {
  Lucene101(Lucene101Codec),
  #[cfg(test)]
  TestLucene90Points(TestLucene90PointsFormatCodec),
  #[cfg(test)]
  Asserting(AssertingCodec),
  #[cfg(test)]
  AssertingNeedsIndexSort(AssertingNeedsIndexSortCodec),
  #[cfg(test)]
  RandomDistance(RandomDistanceCodec),
  #[cfg(test)]
  Compressing(CompressingCodec),
  #[cfg(test)]
  UnRegistered(UnRegisteredCodec),
  #[cfg(test)]
  MergePerField(MergePerFieldCodec),
  #[cfg(test)]
  CrankyLucene101(CrankyCodec<Lucene101Codec>),
  #[cfg(test)]
  CrankyAsserting(CrankyCodec<AssertingCodec>),
  #[cfg(test)]
  InvertedWrite(InvertedWriteCodec),
  #[cfg(test)]
  Minimal(MinimalCodec),
  #[cfg(test)]
  MinimalCompound(MinimalCompoundCodec),
}

impl Default for Codecs {
  fn default() -> Self {
    Self::Lucene101(Lucene101Codec::default())
  }
}

#[cfg(not(test))]
static DEFAULT_CODEC: LazyLock<RwLock<Codecs>> = LazyLock::new(|| RwLock::new(Codecs::default()));

// Rust tests run concurrently in the same process, so their defaults must not interfere with one
// another. This preserves the per-test effect of Java's Codec.setDefault/getDefault lifecycle.
#[cfg(test)]
thread_local! {
  static DEFAULT_CODEC: RefCell<Codecs> = RefCell::new(Codecs::default());
}

impl From<Lucene101Codec> for Codecs {
  fn from(codec: Lucene101Codec) -> Self {
    Self::Lucene101(codec)
  }
}

#[cfg(test)]
impl From<TestLucene90PointsFormatCodec> for Codecs {
  fn from(codec: TestLucene90PointsFormatCodec) -> Self {
    Self::TestLucene90Points(codec)
  }
}

#[cfg(test)]
impl From<AssertingCodec> for Codecs {
  fn from(codec: AssertingCodec) -> Self {
    Self::Asserting(codec)
  }
}

#[cfg(test)]
impl From<AssertingNeedsIndexSortCodec> for Codecs {
  fn from(codec: AssertingNeedsIndexSortCodec) -> Self {
    Self::AssertingNeedsIndexSort(codec)
  }
}

#[cfg(test)]
impl From<RandomDistanceCodec> for Codecs {
  fn from(codec: RandomDistanceCodec) -> Self {
    Self::RandomDistance(codec)
  }
}

#[cfg(test)]
impl From<CompressingCodec> for Codecs {
  fn from(codec: CompressingCodec) -> Self {
    Self::Compressing(codec)
  }
}

#[cfg(test)]
impl From<DummyCompressingCodec> for Codecs {
  fn from(codec: DummyCompressingCodec) -> Self {
    Self::Compressing(codec.into())
  }
}

#[cfg(test)]
impl From<UnRegisteredCodec> for Codecs {
  fn from(codec: UnRegisteredCodec) -> Self {
    Self::UnRegistered(codec)
  }
}

#[cfg(test)]
impl From<MergePerFieldCodec> for Codecs {
  fn from(codec: MergePerFieldCodec) -> Self {
    Self::MergePerField(codec)
  }
}

#[cfg(test)]
impl From<CrankyCodec<Lucene101Codec>> for Codecs {
  fn from(codec: CrankyCodec<Lucene101Codec>) -> Self {
    Self::CrankyLucene101(codec)
  }
}

#[cfg(test)]
impl From<CrankyCodec<AssertingCodec>> for Codecs {
  fn from(codec: CrankyCodec<AssertingCodec>) -> Self {
    Self::CrankyAsserting(codec)
  }
}

#[cfg(test)]
impl From<InvertedWriteCodec> for Codecs {
  fn from(codec: InvertedWriteCodec) -> Self {
    Self::InvertedWrite(codec)
  }
}

#[cfg(test)]
impl From<MinimalCodec> for Codecs {
  fn from(codec: MinimalCodec) -> Self {
    Self::Minimal(codec)
  }
}

#[cfg(test)]
impl From<MinimalCompoundCodec> for Codecs {
  fn from(codec: MinimalCompoundCodec) -> Self {
    Self::MinimalCompound(codec)
  }
}

impl Display for Codecs {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Lucene101(codec) => Display::fmt(codec, f),
      #[cfg(test)]
      Self::TestLucene90Points(codec) => Display::fmt(codec, f),
      #[cfg(test)]
      Self::Asserting(codec) => Display::fmt(codec, f),
      #[cfg(test)]
      Self::AssertingNeedsIndexSort(codec) => Display::fmt(codec, f),
      #[cfg(test)]
      Self::RandomDistance(codec) => Display::fmt(codec, f),
      #[cfg(test)]
      Self::Compressing(codec) => Display::fmt(codec, f),
      #[cfg(test)]
      Self::UnRegistered(codec) => Display::fmt(codec, f),
      #[cfg(test)]
      Self::MergePerField(codec) => Display::fmt(codec, f),
      #[cfg(test)]
      Self::CrankyLucene101(codec) => Display::fmt(codec, f),
      #[cfg(test)]
      Self::CrankyAsserting(codec) => Display::fmt(codec, f),
      #[cfg(test)]
      Self::InvertedWrite(codec) => Display::fmt(codec, f),
      #[cfg(test)]
      Self::Minimal(codec) => Display::fmt(codec, f),
      #[cfg(test)]
      Self::MinimalCompound(codec) => Display::fmt(codec, f),
    }
  }
}

impl Codec for Codecs {
  type PostingsFormat = CodecPostingsFormat;
  type DocValuesFormat = CodecDocValuesFormat;
  type StoredFieldsFormat = CodecStoredFieldsFormat;
  type TermVectorsFormat = CodecTermVectorsFormat;
  #[cfg(not(test))]
  type FieldInfosFormat = <Lucene101Codec as Codec>::FieldInfosFormat;
  #[cfg(test)]
  type FieldInfosFormat = CodecFieldInfosFormat;
  #[cfg(not(test))]
  type SegmentInfoFormat = <Lucene101Codec as Codec>::SegmentInfoFormat;
  #[cfg(test)]
  type SegmentInfoFormat = CodecSegmentInfoFormat;
  type NormsFormat = CodecNormsFormat;
  type LiveDocsFormat = CodecLiveDocsFormat;
  #[cfg(not(test))]
  type CompoundFormat = <Lucene101Codec as Codec>::CompoundFormat;
  #[cfg(test)]
  type CompoundFormat = CodecCompoundFormat;
  type PointsFormat = CodecPointsFormat;
  type KnnVectorsFormat = CodecKnnVectorsFormat;

  fn postings_format(&self) -> Self::PostingsFormat {
    match self {
      Self::Lucene101(codec) => CodecPostingsFormat::Lucene101(codec.postings_format()),
      #[cfg(test)]
      Self::TestLucene90Points(codec) => CodecPostingsFormat::Lucene101(codec.postings_format()),
      #[cfg(test)]
      Self::Asserting(codec) => CodecPostingsFormat::Asserting(codec.postings_format()),
      #[cfg(test)]
      Self::AssertingNeedsIndexSort(codec) => {
        CodecPostingsFormat::Lucene101(codec.postings_format())
      },
      #[cfg(test)]
      Self::RandomDistance(codec) => CodecPostingsFormat::Lucene101(codec.postings_format()),
      #[cfg(test)]
      Self::Compressing(codec) => CodecPostingsFormat::Lucene101(codec.postings_format()),
      #[cfg(test)]
      Self::UnRegistered(codec) => CodecPostingsFormat::Lucene101(codec.postings_format()),
      #[cfg(test)]
      Self::MergePerField(codec) => CodecPostingsFormat::MergePerField(codec.postings_format()),
      #[cfg(test)]
      Self::CrankyLucene101(codec) => CodecPostingsFormat::CrankyLucene101(codec.postings_format()),
      #[cfg(test)]
      Self::CrankyAsserting(codec) => CodecPostingsFormat::CrankyAsserting(codec.postings_format()),
      #[cfg(test)]
      Self::InvertedWrite(codec) => CodecPostingsFormat::InvertedWrite(codec.postings_format()),
      #[cfg(test)]
      Self::Minimal(codec) => CodecPostingsFormat::Lucene101(codec.postings_format()),
      #[cfg(test)]
      Self::MinimalCompound(codec) => CodecPostingsFormat::Lucene101(codec.postings_format()),
    }
  }

  fn doc_values_format(&self) -> Self::DocValuesFormat {
    match self {
      Self::Lucene101(codec) => CodecDocValuesFormat::Lucene101(codec.doc_values_format()),
      #[cfg(test)]
      Self::TestLucene90Points(codec) => CodecDocValuesFormat::Lucene101(codec.doc_values_format()),
      #[cfg(test)]
      Self::Asserting(codec) => CodecDocValuesFormat::Asserting(codec.doc_values_format()),
      #[cfg(test)]
      Self::AssertingNeedsIndexSort(codec) => {
        CodecDocValuesFormat::Lucene101(codec.doc_values_format())
      },
      #[cfg(test)]
      Self::RandomDistance(codec) => CodecDocValuesFormat::Lucene101(codec.doc_values_format()),
      #[cfg(test)]
      Self::Compressing(codec) => CodecDocValuesFormat::Lucene101(codec.doc_values_format()),
      #[cfg(test)]
      Self::UnRegistered(codec) => CodecDocValuesFormat::Lucene101(codec.doc_values_format()),
      #[cfg(test)]
      Self::MergePerField(codec) => CodecDocValuesFormat::MergePerField(codec.doc_values_format()),
      #[cfg(test)]
      Self::CrankyLucene101(codec) => {
        CodecDocValuesFormat::CrankyLucene101(codec.doc_values_format())
      },
      #[cfg(test)]
      Self::CrankyAsserting(codec) => {
        CodecDocValuesFormat::CrankyAsserting(codec.doc_values_format())
      },
      #[cfg(test)]
      Self::InvertedWrite(codec) => codec.doc_values_format(),
      #[cfg(test)]
      Self::Minimal(codec) => CodecDocValuesFormat::Lucene101(codec.doc_values_format()),
      #[cfg(test)]
      Self::MinimalCompound(codec) => CodecDocValuesFormat::Lucene101(codec.doc_values_format()),
    }
  }

  fn stored_fields_format(&self) -> Self::StoredFieldsFormat {
    match self {
      Self::Lucene101(codec) => CodecStoredFieldsFormat::Lucene90(codec.stored_fields_format()),
      #[cfg(test)]
      Self::TestLucene90Points(codec) => {
        CodecStoredFieldsFormat::Lucene90(codec.stored_fields_format())
      },
      #[cfg(test)]
      Self::Asserting(codec) => CodecStoredFieldsFormat::Asserting(codec.stored_fields_format()),
      #[cfg(test)]
      Self::AssertingNeedsIndexSort(codec) => {
        CodecStoredFieldsFormat::Lucene90(codec.stored_fields_format())
      },
      #[cfg(test)]
      Self::RandomDistance(codec) => {
        CodecStoredFieldsFormat::Lucene90(codec.stored_fields_format())
      },
      #[cfg(test)]
      Self::Compressing(codec) => {
        CodecStoredFieldsFormat::Compressing(codec.stored_fields_format())
      },
      #[cfg(test)]
      Self::UnRegistered(codec) => CodecStoredFieldsFormat::Lucene90(codec.stored_fields_format()),
      #[cfg(test)]
      Self::MergePerField(codec) => CodecStoredFieldsFormat::Lucene90(codec.stored_fields_format()),
      #[cfg(test)]
      Self::CrankyLucene101(codec) => {
        CodecStoredFieldsFormat::CrankyLucene101(codec.stored_fields_format())
      },
      #[cfg(test)]
      Self::CrankyAsserting(codec) => {
        CodecStoredFieldsFormat::CrankyAsserting(codec.stored_fields_format())
      },
      #[cfg(test)]
      Self::InvertedWrite(codec) => codec.stored_fields_format(),
      #[cfg(test)]
      Self::Minimal(codec) => CodecStoredFieldsFormat::Lucene90(codec.stored_fields_format()),
      #[cfg(test)]
      Self::MinimalCompound(codec) => {
        CodecStoredFieldsFormat::Lucene90(codec.stored_fields_format())
      },
    }
  }

  fn term_vectors_format(&self) -> Self::TermVectorsFormat {
    match self {
      Self::Lucene101(codec) => CodecTermVectorsFormat::Lucene90(codec.term_vectors_format()),
      #[cfg(test)]
      Self::TestLucene90Points(codec) => {
        CodecTermVectorsFormat::Lucene90(codec.term_vectors_format())
      },
      #[cfg(test)]
      Self::Asserting(codec) => CodecTermVectorsFormat::Asserting(codec.term_vectors_format()),
      #[cfg(test)]
      Self::AssertingNeedsIndexSort(codec) => {
        CodecTermVectorsFormat::Lucene90(codec.term_vectors_format())
      },
      #[cfg(test)]
      Self::RandomDistance(codec) => CodecTermVectorsFormat::Lucene90(codec.term_vectors_format()),
      #[cfg(test)]
      Self::Compressing(codec) => CodecTermVectorsFormat::Compressing(codec.term_vectors_format()),
      #[cfg(test)]
      Self::UnRegistered(codec) => CodecTermVectorsFormat::Lucene90(codec.term_vectors_format()),
      #[cfg(test)]
      Self::MergePerField(codec) => CodecTermVectorsFormat::Lucene90(codec.term_vectors_format()),
      #[cfg(test)]
      Self::CrankyLucene101(codec) => {
        CodecTermVectorsFormat::CrankyLucene101(codec.term_vectors_format())
      },
      #[cfg(test)]
      Self::CrankyAsserting(codec) => {
        CodecTermVectorsFormat::CrankyAsserting(codec.term_vectors_format())
      },
      #[cfg(test)]
      Self::InvertedWrite(codec) => codec.term_vectors_format(),
      #[cfg(test)]
      Self::Minimal(codec) => CodecTermVectorsFormat::Lucene90(codec.term_vectors_format()),
      #[cfg(test)]
      Self::MinimalCompound(codec) => CodecTermVectorsFormat::Lucene90(codec.term_vectors_format()),
    }
  }

  fn field_infos_format(&self) -> Self::FieldInfosFormat {
    match self {
      Self::Lucene101(codec) => {
        #[cfg(not(test))]
        {
          codec.field_infos_format()
        }
        #[cfg(test)]
        {
          CodecFieldInfosFormat::Lucene101(codec.field_infos_format())
        }
      },
      #[cfg(test)]
      Self::TestLucene90Points(codec) => {
        CodecFieldInfosFormat::Lucene101(codec.field_infos_format())
      },
      #[cfg(test)]
      Self::Asserting(codec) => CodecFieldInfosFormat::Lucene101(codec.field_infos_format()),
      #[cfg(test)]
      Self::AssertingNeedsIndexSort(codec) => {
        CodecFieldInfosFormat::Lucene101(codec.field_infos_format())
      },
      #[cfg(test)]
      Self::RandomDistance(codec) => CodecFieldInfosFormat::Lucene101(codec.field_infos_format()),
      #[cfg(test)]
      Self::Compressing(codec) => CodecFieldInfosFormat::Lucene101(codec.field_infos_format()),
      #[cfg(test)]
      Self::UnRegistered(codec) => CodecFieldInfosFormat::Lucene101(codec.field_infos_format()),
      #[cfg(test)]
      Self::MergePerField(codec) => CodecFieldInfosFormat::Lucene101(codec.field_infos_format()),
      #[cfg(test)]
      Self::CrankyLucene101(codec) => CodecFieldInfosFormat::Cranky(codec.field_infos_format()),
      #[cfg(test)]
      Self::CrankyAsserting(codec) => CodecFieldInfosFormat::Cranky(codec.field_infos_format()),
      #[cfg(test)]
      Self::InvertedWrite(codec) => codec.field_infos_format(),
      #[cfg(test)]
      Self::Minimal(codec) => CodecFieldInfosFormat::Lucene101(codec.field_infos_format()),
      #[cfg(test)]
      Self::MinimalCompound(codec) => CodecFieldInfosFormat::Lucene101(codec.field_infos_format()),
    }
  }

  fn segment_info_format(&self) -> Self::SegmentInfoFormat {
    match self {
      Self::Lucene101(codec) => {
        #[cfg(not(test))]
        {
          codec.segment_info_format()
        }
        #[cfg(test)]
        {
          CodecSegmentInfoFormat::Lucene101(codec.segment_info_format())
        }
      },
      #[cfg(test)]
      Self::TestLucene90Points(codec) => {
        CodecSegmentInfoFormat::Lucene101(codec.segment_info_format())
      },
      #[cfg(test)]
      Self::Asserting(codec) => CodecSegmentInfoFormat::Lucene101(codec.segment_info_format()),
      #[cfg(test)]
      Self::AssertingNeedsIndexSort(codec) => {
        CodecSegmentInfoFormat::Lucene101(codec.segment_info_format())
      },
      #[cfg(test)]
      Self::RandomDistance(codec) => CodecSegmentInfoFormat::Lucene101(codec.segment_info_format()),
      #[cfg(test)]
      Self::Compressing(codec) => CodecSegmentInfoFormat::Lucene101(codec.segment_info_format()),
      #[cfg(test)]
      Self::UnRegistered(codec) => CodecSegmentInfoFormat::Lucene101(codec.segment_info_format()),
      #[cfg(test)]
      Self::MergePerField(codec) => CodecSegmentInfoFormat::Lucene101(codec.segment_info_format()),
      #[cfg(test)]
      Self::CrankyLucene101(codec) => CodecSegmentInfoFormat::Cranky(codec.segment_info_format()),
      #[cfg(test)]
      Self::CrankyAsserting(codec) => CodecSegmentInfoFormat::Cranky(codec.segment_info_format()),
      #[cfg(test)]
      Self::InvertedWrite(codec) => codec.segment_info_format(),
      #[cfg(test)]
      Self::Minimal(codec) => CodecSegmentInfoFormat::Lucene101(codec.segment_info_format()),
      #[cfg(test)]
      Self::MinimalCompound(codec) => {
        CodecSegmentInfoFormat::Lucene101(codec.segment_info_format())
      },
    }
  }

  fn norms_format(&self) -> Self::NormsFormat {
    match self {
      Self::Lucene101(codec) => CodecNormsFormat::Lucene90(codec.norms_format()),
      #[cfg(test)]
      Self::TestLucene90Points(codec) => CodecNormsFormat::Lucene90(codec.norms_format()),
      #[cfg(test)]
      Self::Asserting(codec) => CodecNormsFormat::Asserting(codec.norms_format()),
      #[cfg(test)]
      Self::AssertingNeedsIndexSort(codec) => CodecNormsFormat::Lucene90(codec.norms_format()),
      #[cfg(test)]
      Self::RandomDistance(codec) => CodecNormsFormat::Lucene90(codec.norms_format()),
      #[cfg(test)]
      Self::Compressing(codec) => CodecNormsFormat::Lucene90(codec.norms_format()),
      #[cfg(test)]
      Self::UnRegistered(codec) => CodecNormsFormat::Lucene90(codec.norms_format()),
      #[cfg(test)]
      Self::MergePerField(codec) => CodecNormsFormat::Lucene90(codec.norms_format()),
      #[cfg(test)]
      Self::CrankyLucene101(codec) => CodecNormsFormat::CrankyLucene101(codec.norms_format()),
      #[cfg(test)]
      Self::CrankyAsserting(codec) => CodecNormsFormat::CrankyAsserting(codec.norms_format()),
      #[cfg(test)]
      Self::InvertedWrite(codec) => codec.norms_format(),
      #[cfg(test)]
      Self::Minimal(codec) => CodecNormsFormat::Lucene90(codec.norms_format()),
      #[cfg(test)]
      Self::MinimalCompound(codec) => CodecNormsFormat::Lucene90(codec.norms_format()),
    }
  }

  fn live_docs_format(&self) -> Self::LiveDocsFormat {
    match self {
      Self::Lucene101(codec) => CodecLiveDocsFormat::Lucene90(codec.live_docs_format()),
      #[cfg(test)]
      Self::TestLucene90Points(codec) => CodecLiveDocsFormat::Lucene90(codec.live_docs_format()),
      #[cfg(test)]
      Self::Asserting(codec) => CodecLiveDocsFormat::Asserting(codec.live_docs_format()),
      #[cfg(test)]
      Self::AssertingNeedsIndexSort(codec) => {
        CodecLiveDocsFormat::Lucene90(codec.live_docs_format())
      },
      #[cfg(test)]
      Self::RandomDistance(codec) => CodecLiveDocsFormat::Lucene90(codec.live_docs_format()),
      #[cfg(test)]
      Self::Compressing(codec) => CodecLiveDocsFormat::Lucene90(codec.live_docs_format()),
      #[cfg(test)]
      Self::UnRegistered(codec) => CodecLiveDocsFormat::Lucene90(codec.live_docs_format()),
      #[cfg(test)]
      Self::MergePerField(codec) => CodecLiveDocsFormat::Lucene90(codec.live_docs_format()),
      #[cfg(test)]
      Self::CrankyLucene101(codec) => {
        CodecLiveDocsFormat::CrankyLucene101(codec.live_docs_format())
      },
      #[cfg(test)]
      Self::CrankyAsserting(codec) => {
        CodecLiveDocsFormat::CrankyAsserting(codec.live_docs_format())
      },
      #[cfg(test)]
      Self::InvertedWrite(codec) => codec.live_docs_format(),
      #[cfg(test)]
      Self::Minimal(codec) => CodecLiveDocsFormat::Lucene90(codec.live_docs_format()),
      #[cfg(test)]
      Self::MinimalCompound(codec) => CodecLiveDocsFormat::Lucene90(codec.live_docs_format()),
    }
  }

  fn compound_format(&self) -> Self::CompoundFormat {
    match self {
      Self::Lucene101(codec) => {
        #[cfg(not(test))]
        {
          codec.compound_format()
        }
        #[cfg(test)]
        {
          CodecCompoundFormat::Lucene101(codec.compound_format())
        }
      },
      #[cfg(test)]
      Self::TestLucene90Points(codec) => CodecCompoundFormat::Lucene101(codec.compound_format()),
      #[cfg(test)]
      Self::Asserting(codec) => CodecCompoundFormat::Lucene101(codec.compound_format()),
      #[cfg(test)]
      Self::AssertingNeedsIndexSort(codec) => {
        CodecCompoundFormat::Lucene101(codec.compound_format())
      },
      #[cfg(test)]
      Self::RandomDistance(codec) => CodecCompoundFormat::Lucene101(codec.compound_format()),
      #[cfg(test)]
      Self::Compressing(codec) => CodecCompoundFormat::Lucene101(codec.compound_format()),
      #[cfg(test)]
      Self::UnRegistered(codec) => CodecCompoundFormat::Lucene101(codec.compound_format()),
      #[cfg(test)]
      Self::MergePerField(codec) => CodecCompoundFormat::Lucene101(codec.compound_format()),
      #[cfg(test)]
      Self::CrankyLucene101(codec) => CodecCompoundFormat::Cranky(codec.compound_format()),
      #[cfg(test)]
      Self::CrankyAsserting(codec) => CodecCompoundFormat::Cranky(codec.compound_format()),
      #[cfg(test)]
      Self::InvertedWrite(codec) => codec.compound_format(),
      #[cfg(test)]
      Self::Minimal(codec) => CodecCompoundFormat::Lucene101(codec.compound_format()),
      #[cfg(test)]
      Self::MinimalCompound(codec) => CodecCompoundFormat::Lucene101(codec.compound_format()),
    }
  }

  fn points_format(&self) -> Self::PointsFormat {
    match self {
      Self::Lucene101(codec) => CodecPointsFormat::Lucene90(codec.points_format()),
      #[cfg(test)]
      Self::TestLucene90Points(codec) => CodecPointsFormat::TestLucene90(codec.points_format()),
      #[cfg(test)]
      Self::Asserting(codec) => CodecPointsFormat::Asserting(codec.points_format()),
      #[cfg(test)]
      Self::AssertingNeedsIndexSort(codec) => {
        CodecPointsFormat::AssertingNeedsIndexSort(codec.points_format())
      },
      #[cfg(test)]
      Self::RandomDistance(codec) => CodecPointsFormat::RandomDistance(codec.points_format()),
      #[cfg(test)]
      Self::Compressing(codec) => CodecPointsFormat::Lucene90(codec.points_format()),
      #[cfg(test)]
      Self::UnRegistered(codec) => CodecPointsFormat::Lucene90(codec.points_format()),
      #[cfg(test)]
      Self::MergePerField(codec) => CodecPointsFormat::Lucene90(codec.points_format()),
      #[cfg(test)]
      Self::CrankyLucene101(codec) => CodecPointsFormat::CrankyLucene101(codec.points_format()),
      #[cfg(test)]
      Self::CrankyAsserting(codec) => CodecPointsFormat::CrankyAsserting(codec.points_format()),
      #[cfg(test)]
      Self::InvertedWrite(codec) => codec.points_format(),
      #[cfg(test)]
      Self::Minimal(codec) => CodecPointsFormat::Lucene90(codec.points_format()),
      #[cfg(test)]
      Self::MinimalCompound(codec) => CodecPointsFormat::Lucene90(codec.points_format()),
    }
  }

  fn knn_vectors_format(&self) -> Result<Self::KnnVectorsFormat> {
    match self {
      Self::Lucene101(codec) => codec
        .knn_vectors_format()
        .map(CodecKnnVectorsFormat::Lucene101),
      #[cfg(test)]
      Self::TestLucene90Points(codec) => codec
        .knn_vectors_format()
        .map(CodecKnnVectorsFormat::Lucene101),
      #[cfg(test)]
      Self::Asserting(codec) => codec
        .knn_vectors_format()
        .map(CodecKnnVectorsFormat::Asserting),
      #[cfg(test)]
      Self::AssertingNeedsIndexSort(codec) => codec
        .knn_vectors_format()
        .map(CodecKnnVectorsFormat::Lucene101),
      #[cfg(test)]
      Self::RandomDistance(codec) => codec
        .knn_vectors_format()
        .map(CodecKnnVectorsFormat::Lucene101),
      #[cfg(test)]
      Self::Compressing(codec) => codec
        .knn_vectors_format()
        .map(CodecKnnVectorsFormat::Lucene101),
      #[cfg(test)]
      Self::UnRegistered(codec) => codec
        .knn_vectors_format()
        .map(CodecKnnVectorsFormat::Lucene101),
      #[cfg(test)]
      Self::MergePerField(codec) => codec
        .knn_vectors_format()
        .map(CodecKnnVectorsFormat::Lucene101),
      #[cfg(test)]
      Self::CrankyLucene101(codec) => codec
        .knn_vectors_format()
        .map(CodecKnnVectorsFormat::Lucene101),
      #[cfg(test)]
      Self::CrankyAsserting(codec) => codec
        .knn_vectors_format()
        .map(CodecKnnVectorsFormat::Asserting),
      #[cfg(test)]
      Self::InvertedWrite(codec) => codec.knn_vectors_format(),
      #[cfg(test)]
      Self::Minimal(codec) => codec
        .knn_vectors_format()
        .map(CodecKnnVectorsFormat::Lucene101),
      #[cfg(test)]
      Self::MinimalCompound(codec) => codec
        .knn_vectors_format()
        .map(CodecKnnVectorsFormat::Lucene101),
    }
  }

  fn get_name(&self) -> &str {
    match self {
      Self::Lucene101(codec) => codec.get_name(),
      #[cfg(test)]
      Self::TestLucene90Points(codec) => codec.get_name(),
      #[cfg(test)]
      Self::Asserting(codec) => codec.get_name(),
      #[cfg(test)]
      Self::AssertingNeedsIndexSort(codec) => codec.get_name(),
      #[cfg(test)]
      Self::RandomDistance(codec) => codec.get_name(),
      #[cfg(test)]
      Self::Compressing(codec) => codec.get_name(),
      #[cfg(test)]
      Self::UnRegistered(codec) => codec.get_name(),
      #[cfg(test)]
      Self::MergePerField(codec) => codec.get_name(),
      #[cfg(test)]
      Self::CrankyLucene101(codec) => codec.get_name(),
      #[cfg(test)]
      Self::CrankyAsserting(codec) => codec.get_name(),
      #[cfg(test)]
      Self::InvertedWrite(codec) => codec.get_name(),
      #[cfg(test)]
      Self::Minimal(codec) => codec.get_name(),
      #[cfg(test)]
      Self::MinimalCompound(codec) => codec.get_name(),
    }
  }
}

/// Returns the current default codec.
///
/// This mirrors Java Lucene's `Codec.getDefault` entry point. The initial default is `Lucene101`.
pub fn get_default() -> Codecs {
  #[cfg(not(test))]
  {
    DEFAULT_CODEC.read().clone()
  }
  #[cfg(test)]
  {
    DEFAULT_CODEC.with(|codec| codec.borrow().clone())
  }
}

/// Sets the default codec used by newly created index writer configurations.
///
/// This mirrors Java Lucene's `Codec.setDefault` entry point.
pub fn set_default(codec: impl Into<Codecs>) {
  let codec = codec.into();
  #[cfg(not(test))]
  {
    *DEFAULT_CODEC.write() = codec;
  }
  #[cfg(test)]
  {
    DEFAULT_CODEC.with(|default_codec| *default_codec.borrow_mut() = codec);
  }
}

/// Looks up a codec by name.
///
/// This mirrors Java Lucene's `Codec.forName` entry point.
pub fn for_name(name: &str) -> Result<Codecs> {
  match name {
    "Lucene101" => Ok(Codecs::default()),
    #[cfg(test)]
    "Asserting" => Ok(Codecs::Asserting(AssertingCodec::new())),
    #[cfg(test)]
    "MinimalCodec" => Ok(MinimalCodec::new().into()),
    #[cfg(test)]
    "MinimalCompoundCodec" => Ok(MinimalCompoundCodec::new().into()),
    #[cfg(test)]
    "FastCompressingStoredFieldsData"
    | "FastDecompressionCompressingStoredFieldsData"
    | "HighCompressionCompressingStoredFieldsData"
    | "DummyCompressingStoredFieldsData"
    | "DeflateWithPresetCompressingStoredFieldsData"
    | "LZ4WithPresetCompressingStoredFieldsData" => {
      CompressingCodec::for_name(name).map(Codecs::Compressing)
    },
    _ => Err(LuceneError::illegal_argument(format!(
      "Could not load codec named \"{}\"",
      name
    ))),
  }
}

pub type DefaultPostingsFormat = <Lucene101Codec as Codec>::PostingsFormat;
pub type DefaultDocValuesFormat = <Lucene101Codec as Codec>::DocValuesFormat;
pub type DefaultStoredFieldsFormat = <Lucene101Codec as Codec>::StoredFieldsFormat;
pub type DefaultTermVectorsFormat = <Lucene101Codec as Codec>::TermVectorsFormat;
pub type DefaultFieldInfosFormat = <Lucene101Codec as Codec>::FieldInfosFormat;
pub type DefaultSegmentInfoFormat = <Lucene101Codec as Codec>::SegmentInfoFormat;
pub type DefaultNormsFormat = <Lucene101Codec as Codec>::NormsFormat;
pub type DefaultLiveDocsFormat = <Lucene101Codec as Codec>::LiveDocsFormat;
pub type DefaultCompoundFormat = <Lucene101Codec as Codec>::CompoundFormat;
pub type DefaultPointsFormat = <Lucene101Codec as Codec>::PointsFormat;
pub type DefaultCodecKnnVectorsFormat = <Lucene101Codec as Codec>::KnnVectorsFormat;
