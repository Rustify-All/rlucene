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
use crate::core::codecs::knn_vectors_formats::KnnVectorsFormats;
use crate::core::codecs::lucene90::lucene90_compound_format::Lucene90CompoundFormat;
use crate::core::codecs::lucene90_doc_values_format::Lucene90DocValuesFormat;
use crate::core::codecs::lucene90_live_docs_format::Lucene90LiveDocsFormat;
use crate::core::codecs::lucene90_norms_format::Lucene90NormsFormat;
use crate::core::codecs::lucene90_points_format::Lucene90PointsFormat;
use crate::core::codecs::lucene90_stored_fields_format::{
  Lucene90StoredFieldsFormat, Mode as StoredFieldsMode,
};
use crate::core::codecs::lucene90_term_vectors_format::Lucene90TermVectorsFormat;
use crate::core::codecs::lucene94::lucene94_field_infos_format::Lucene94FieldInfosFormat;
use crate::core::codecs::lucene99::lucene99_hnsw_vectors_format::Lucene99HnswVectorsFormat;
use crate::core::codecs::lucene99::lucene99_segment_info_format::Lucene99SegmentInfoFormat;
use crate::core::codecs::lucene101::lucene101_postings_format::Lucene101PostingsFormat;
use crate::core::codecs::perfield::per_field_doc_values_format::{
  PerFieldDocValuesFormat, PerFieldDocValuesFormatBase,
};
use crate::core::codecs::perfield::per_field_knn_vectors_format::{
  PerFieldKnnVectorsFormat, PerFieldKnnVectorsFormatBase,
};
use crate::core::codecs::perfield::per_field_postings_format::{
  PerFieldPostingsFormat, PerFieldPostingsFormatBase,
};
use crate::core::util::error::lucene_error::Result;
use std::fmt::{Display, Formatter};

/// Configuration option for the codec.
#[derive(Clone, Copy)]
pub enum Mode {
  /// Trade compression ratio for retrieval speed.
  BestSpeed,
  /// Trade retrieval speed for compression ratio.
  BestCompression,
}

impl Mode {
  fn stored_mode(self) -> StoredFieldsMode {
    match self {
      Self::BestSpeed => StoredFieldsMode::BestSpeed,
      Self::BestCompression => StoredFieldsMode::BestCompression,
    }
  }
}

type DefaultPostingsFormat = Lucene101PostingsFormat;
type DefaultDocValuesFormat = Lucene90DocValuesFormat;

pub type Lucene101CodecPostingsFormat = PerFieldPostingsFormat<Lucene101CodecPostingsFormatBase>;
pub type Lucene101CodecDocValuesFormat = PerFieldDocValuesFormat<Lucene101CodecDocValuesFormatBase>;
pub type Lucene101CodecKnnVectorsFormat =
  PerFieldKnnVectorsFormat<Lucene101CodecKnnVectorsFormatBase>;

pub struct Lucene101CodecPostingsFormatBase {
  default_postings_format: DefaultPostingsFormat,
}

impl PerFieldPostingsFormatBase for Lucene101CodecPostingsFormatBase {
  type Format = DefaultPostingsFormat;

  fn get_postings_format_for_field(&self, _field: &str) -> Result<&Self::Format> {
    Ok(&self.default_postings_format)
  }
}

pub struct Lucene101CodecDocValuesFormatBase {
  default_doc_values_format: DefaultDocValuesFormat,
}

impl PerFieldDocValuesFormatBase for Lucene101CodecDocValuesFormatBase {
  type Format = DefaultDocValuesFormat;

  fn get_doc_values_format_for_field(&self, _field: &str) -> Result<&Self::Format> {
    Ok(&self.default_doc_values_format)
  }
}

pub struct Lucene101CodecKnnVectorsFormatBase {
  default_knn_vectors_format: KnnVectorsFormats,
}

impl PerFieldKnnVectorsFormatBase for Lucene101CodecKnnVectorsFormatBase {
  type Format = KnnVectorsFormats;

  fn get_knn_vectors_format_for_field(&self, _field: &str) -> Result<&Self::Format> {
    Ok(&self.default_knn_vectors_format)
  }
}

#[derive(Clone)]
pub struct Lucene101Codec {
  postings_format: Lucene101CodecPostingsFormat,
  doc_values_format: Lucene101CodecDocValuesFormat,
  knn_vectors_format: Lucene101CodecKnnVectorsFormat,
  stored_fields_format: Lucene90StoredFieldsFormat,
}

impl Default for Lucene101Codec {
  fn default() -> Self {
    Self::with_mode(Mode::BestSpeed)
  }
}

impl Lucene101Codec {
  /// Instantiates a new codec, specifying the stored fields compression mode to use.
  pub fn with_mode(mode: Mode) -> Self {
    Self {
      stored_fields_format: Lucene90StoredFieldsFormat::with_mode(mode.stored_mode()),
      postings_format: PerFieldPostingsFormat::new(Lucene101CodecPostingsFormatBase {
        default_postings_format: DefaultPostingsFormat::new(),
      }),
      doc_values_format: PerFieldDocValuesFormat::new(Lucene101CodecDocValuesFormatBase {
        default_doc_values_format: DefaultDocValuesFormat::default(),
      }),
      knn_vectors_format: PerFieldKnnVectorsFormat::new(Lucene101CodecKnnVectorsFormatBase {
        default_knn_vectors_format: Lucene99HnswVectorsFormat::new()
          .expect("default KNN vectors format parameters are valid")
          .into(),
      }),
    }
  }
}

impl Display for Lucene101Codec {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "Lucene101Codec")
  }
}
impl Codec for Lucene101Codec {
  type PostingsFormat = Lucene101CodecPostingsFormat;
  type DocValuesFormat = Lucene101CodecDocValuesFormat;
  type StoredFieldsFormat = Lucene90StoredFieldsFormat;
  type TermVectorsFormat = Lucene90TermVectorsFormat;
  type FieldInfosFormat = Lucene94FieldInfosFormat;
  type SegmentInfoFormat = Lucene99SegmentInfoFormat;
  type NormsFormat = Lucene90NormsFormat;
  type LiveDocsFormat = Lucene90LiveDocsFormat;
  type CompoundFormat = Lucene90CompoundFormat;
  type PointsFormat = Lucene90PointsFormat;
  type KnnVectorsFormat = Lucene101CodecKnnVectorsFormat;

  fn postings_format(&self) -> Self::PostingsFormat {
    self.postings_format.clone()
  }

  fn doc_values_format(&self) -> Self::DocValuesFormat {
    self.doc_values_format.clone()
  }

  fn stored_fields_format(&self) -> Self::StoredFieldsFormat {
    self.stored_fields_format.clone()
  }

  fn term_vectors_format(&self) -> Self::TermVectorsFormat {
    Lucene90TermVectorsFormat::default()
  }

  fn field_infos_format(&self) -> Self::FieldInfosFormat {
    Lucene94FieldInfosFormat
  }

  fn segment_info_format(&self) -> Self::SegmentInfoFormat {
    Lucene99SegmentInfoFormat
  }

  fn norms_format(&self) -> Self::NormsFormat {
    Lucene90NormsFormat
  }

  fn live_docs_format(&self) -> Self::LiveDocsFormat {
    Lucene90LiveDocsFormat
  }

  fn compound_format(&self) -> Self::CompoundFormat {
    Lucene90CompoundFormat
  }

  fn points_format(&self) -> Self::PointsFormat {
    Lucene90PointsFormat
  }

  fn knn_vectors_format(&self) -> Result<Self::KnnVectorsFormat> {
    Ok(self.knn_vectors_format.clone())
  }

  fn get_name(&self) -> &str {
    "Lucene101"
  }
}
