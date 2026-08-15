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
use crate::core::codecs::doc_values_producer::{DocValuesProducer, DocValuesProducerEnum2};
use crate::core::codecs::fields_producer::{FieldsProducer, FieldsProducerEnum2};
use crate::core::codecs::knn_vectors_reader::{KnnVectorsReader, KnnVectorsReaderEnum2};
use crate::core::codecs::norms_producer::{NormsProducer, NormsProducerEnum2};
use crate::core::codecs::points_reader::{PointsReader, PointsReaderEnum2};
use crate::core::codecs::stored_fields_reader::{
  DefaultStoredFieldsReader, StoredFieldsReader, StoredFieldsReaderEnum2,
};
use crate::core::codecs::stored_fields_writer::StoredFieldsWriter;
use crate::core::codecs::term_vectors_reader::{TermVectorsReader, TermVectorsReaderEnum2};
use crate::core::index::binary_doc_values::BinaryDocValuesEnum2;
use crate::core::index::byte_vector_values::ByteVectorValuesEnum2;
use crate::core::index::doc_values_skip_index_type::DocValuesSkipIndexType;
use crate::core::index::doc_values_skipper::DocValuesSkipperEnum2;
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::fields::Fields;
use crate::core::index::filter_directory_reader::DelegatingCacheHelper;
use crate::core::index::float_vector_values::FloatVectorValuesEnum2;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::index_reader::{CacheHelperEnum2, IndexReader, LeafReaderContextKind};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::numeric_doc_values::NumericDocValuesEnum2;
use crate::core::index::point_values::PointValuesEnum2;
use crate::core::index::sorted_doc_values::SortedDocValuesEnum2;
use crate::core::index::sorted_numeric_doc_values::SortedNumericDocValuesEnum2;
use crate::core::index::sorted_set_doc_values_writer::SortedSetDocValuesEnum2;
use crate::core::index::stored_field_visitor::StoredFieldVisitor;
use crate::core::index::stored_fields::{RawStoredFieldsReader, StoredFields, StoredFieldsEnum2};
use crate::core::index::term_vectors::{EmptyTermVectors, RawTermVectors, TermVectorsEnum2};
use crate::core::index::terms::TermsEnum2;
use crate::core::index::vector_encoding::VectorEncoding;
use crate::core::search::knn_collector::KnnCollector;
use crate::core::util::CoreHelper;
use crate::core::util::bits::{Bits, BitsEnum2};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::FixedBitSet;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

/// LeafReader implemented by codec APIs.
pub trait CodecReader: LeafReader {
  type StoredFieldsReader: StoredFieldsReader;
  type TermVectorsReader: TermVectorsReader;
  type NormsProducer: NormsProducer;
  type DocValuesProducer: DocValuesProducer;
  type FieldsProducer: FieldsProducer;
  type PointsReader: PointsReader;
  type KnnVectorsReader: KnnVectorsReader;

  /// Expert: retrieve underlying StoredFieldsReader
  fn get_fields_reader(&self) -> Result<Option<Self::StoredFieldsReader>>;

  /// Expert: retrieve underlying TermVectorsReader
  fn get_term_vectors_reader(&self) -> Result<Option<Self::TermVectorsReader>>;

  /// Expert: retrieve underlying NormsProducer
  fn get_norms_reader(&self) -> Result<Option<Self::NormsProducer>>;

  /// Expert: retrieve underlying DocValuesProducer
  fn get_doc_values_reader(&self) -> Result<Option<Self::DocValuesProducer>>;

  /// Expert: retrieve underlying FieldsProducer (postings)
  fn get_postings_reader(&self) -> Result<Option<Self::FieldsProducer>>;

  /// Expert: retrieve underlying PointsReader
  fn get_points_reader(&self) -> Result<Option<Self::PointsReader>>;

  /// retrieve underlying VectorReader
  fn get_vector_reader(&self) -> Result<Option<Self::KnnVectorsReader>>;

  fn stored_fields(&self) -> Result<StoredFieldsType<Self::StoredFieldsReader>> {
    let reader = self.get_fields_reader()?;
    match reader {
      None => Err(LuceneError::illegal_state(
        "stored fields reader is None".to_string(),
      )),
      Some(r) => Ok(StoredFieldsImpl {
        reader: r,
        max_doc: self.max_doc()?,
      }),
    }
  }

  fn term_vectors(&self) -> Result<TermVectorsType<Self::TermVectorsReader>> {
    let reader = self.get_term_vectors_reader()?;
    match reader {
      Some(r) => Ok(TermVectorsEnum2::B(r)),
      None => Ok(TermVectorsEnum2::A(EmptyTermVectors)),
    }
  }
  fn terms(
    &self,
    field: &str,
  ) -> Result<Option<<<Self as CodecReader>::FieldsProducer as Fields>::Terms>> {
    self.ensure_open()?;
    let fi = self.get_field_infos()?.field_info_by_name(field)?;

    if fi.is_none() || *fi.unwrap().get_index_options() == IndexOptions::None {
      // Field does not exist or does not index postings
      return Ok(None);
    }
    match self.get_postings_reader()? {
      None => Err(LuceneError::illegal_state(
        "postings reader is None".to_string(),
      )),
      Some(p) => p.terms(field),
    }
  }
  fn get_dv_field(&self, field: &str, ty: DocValuesType) -> Result<Option<Arc<FieldInfo>>> {
    let fi = self.get_field_infos()?.field_info_by_name(field)?;

    let fi = match fi {
      Some(f) => f,
      None => return Ok(None), // Field does not exist
    };

    if *fi.get_doc_values_type() == DocValuesType::None {
      // Field was not indexed with doc values
      return Ok(None);
    }

    if *fi.get_doc_values_type() != ty {
      // Field DocValues are different than requested type
      return Ok(None);
    }

    Ok(Some(fi))
  }

  fn get_numeric_doc_values(
    &self,
    field: &str,
  ) -> Result<Option<<Self::DocValuesProducer as DocValuesProducer>::NumericDocValues>> {
    self.ensure_open()?;

    let fi = self.get_dv_field(field, DocValuesType::Numeric)?;
    let fi = match fi {
      Some(f) => f,
      None => return Ok(None),
    };
    let reader = self
      .get_doc_values_reader()?
      .ok_or_else(|| LuceneError::illegal_state("doc values reader is None"))?;

    Ok(Some(reader.get_numeric(&fi)?))
  }
  fn get_binary_doc_values(
    &self,
    field: &str,
  ) -> Result<Option<<Self::DocValuesProducer as DocValuesProducer>::BinaryDocValues>> {
    let fi = self.get_dv_field(field, DocValuesType::Binary)?;
    let fi = match fi {
      Some(f) => f,
      None => return Ok(None),
    };

    let reader = self
      .get_doc_values_reader()?
      .ok_or_else(|| LuceneError::illegal_state("doc values reader is None"))?;

    Ok(Some(reader.get_binary(&fi)?))
  }

  fn get_sorted_doc_values(
    &self,
    field: &str,
  ) -> Result<Option<<Self::DocValuesProducer as DocValuesProducer>::SortedDocValues>> {
    let fi = self.get_dv_field(field, DocValuesType::Sorted)?;
    let fi = match fi {
      Some(f) => f,
      None => return Ok(None),
    };
    let reader = self
      .get_doc_values_reader()?
      .ok_or_else(|| LuceneError::illegal_state("doc values reader is None"))?;

    Ok(Some(reader.get_sorted(&fi)?))
  }

  fn get_sorted_numeric_doc_values(
    &self,
    field: &str,
  ) -> Result<Option<<Self::DocValuesProducer as DocValuesProducer>::SortedNumericDocValues>> {
    let fi = self.get_dv_field(field, DocValuesType::SortedNumeric)?;
    let fi = match fi {
      Some(f) => f,
      None => return Ok(None),
    };

    let reader = self
      .get_doc_values_reader()?
      .ok_or_else(|| LuceneError::illegal_state("doc values reader is None"))?;

    Ok(Some(reader.get_sorted_numeric(&fi)?))
  }
  fn get_sorted_set_doc_values(
    &self,
    field: &str,
  ) -> Result<Option<<Self::DocValuesProducer as DocValuesProducer>::SortedSetDocValues>> {
    let fi = self.get_dv_field(field, DocValuesType::SortedSet)?;
    let fi = match fi {
      Some(f) => f,
      None => return Ok(None),
    };
    let reader = self
      .get_doc_values_reader()?
      .ok_or_else(|| LuceneError::illegal_state("doc values reader is None"))?;

    Ok(Some(reader.get_sorted_set(&fi)?))
  }
  fn get_doc_values_skipper(
    &self,
    field: &str,
  ) -> Result<Option<<Self::DocValuesProducer as DocValuesProducer>::DocValuesSkipper>> {
    self.ensure_open()?;

    let fi = self.get_field_infos()?.field_info_by_name(field)?;
    let fi = match fi {
      Some(f) if *f.doc_values_skip_index_type() != DocValuesSkipIndexType::None => f,
      _ => return Ok(None),
    };
    let reader = self
      .get_doc_values_reader()?
      .ok_or_else(|| LuceneError::illegal_state("doc values reader is None"))?;

    reader.get_skipper(&fi)
  }

  fn get_norm_values(
    &self,
    field: &str,
  ) -> Result<Option<<Self::NormsProducer as NormsProducer>::NumericDocValues>> {
    self.ensure_open()?;

    let fi = self.get_field_infos()?.field_info_by_name(field)?;
    let fi = match fi {
      Some(f) if f.has_norms() => f,
      _ => return Ok(None),
    };
    let reader = self
      .get_norms_reader()?
      .ok_or_else(|| LuceneError::illegal_state("norms reader is None"))?;

    Ok(Some(reader.get_norms(&fi)?))
  }
  fn get_point_values(
    &self,
    field: &str,
  ) -> Result<Option<<Self::PointsReader as PointsReader>::PointValuesType>> {
    self.ensure_open()?;

    let fi = self.get_field_infos()?.field_info_by_name(field)?;
    match fi {
      Some(f) if f.get_point_dimension_count() > 0 => f,
      _ => return Ok(None),
    };
    let reader = self
      .get_points_reader()?
      .ok_or_else(|| LuceneError::illegal_state("points reader is None"))?;

    reader.get_values(field)
  }

  fn get_float_vector_values(
    &self,
    field: &str,
  ) -> Result<Option<<Self::KnnVectorsReader as KnnVectorsReader>::FloatVectorValues>> {
    self.ensure_open()?;

    let fi = self.get_field_infos()?.field_info_by_name(field)?;
    match fi {
      Some(f)
        if f.get_vector_dimension() > 0
          && *f.get_vector_encoding() == VectorEncoding::FLOAT32(4) => {},
      _ => return Ok(None),
    };

    let reader = self
      .get_vector_reader()?
      .ok_or_else(|| LuceneError::illegal_state("vector reader is None"))?;

    Ok(Some(reader.get_float_vector_values(field)?))
  }
  fn get_byte_vector_values(
    &self,
    field: &str,
  ) -> Result<Option<<Self::KnnVectorsReader as KnnVectorsReader>::ByteVectorValues>> {
    self.ensure_open()?;

    let fi = self.get_field_infos()?.field_info_by_name(field)?;
    match fi {
      Some(f)
        if f.get_vector_dimension() > 0 && *f.get_vector_encoding() == VectorEncoding::BYTE(1) => {
      },
      _ => return Ok(None),
    };

    let reader = self
      .get_vector_reader()?
      .ok_or_else(|| LuceneError::illegal_state("vector reader is None"))?;

    Ok(Some(reader.get_byte_vector_values(field)?))
  }

  fn search_nearest_vectors_f32<B, K>(
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
    self.ensure_open()?;

    let fi = self.get_field_infos()?.field_info_by_name(field)?;
    match fi {
      Some(f)
        if f.get_vector_dimension() > 0
          && *f.get_vector_encoding() == VectorEncoding::FLOAT32(4) => {},
      _ => return Ok(()),
    };

    let reader = self
      .get_vector_reader()?
      .ok_or_else(|| LuceneError::illegal_state("vector reader is None"))?;
    reader.search_f32(field, target, knn_collector, accept_docs)
  }

  fn search_nearest_vectors_u8<B, K>(
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
    self.ensure_open()?;

    let fi = self.get_field_infos()?.field_info_by_name(field)?;
    match fi {
      Some(f)
        if f.get_vector_dimension() > 0 && *f.get_vector_encoding() == VectorEncoding::BYTE(1) => {
      },
      _ => return Ok(()),
    };

    let reader = self
      .get_vector_reader()?
      .ok_or_else(|| LuceneError::illegal_state("vector reader is None"))?;
    reader.search_u8(field, target, knn_collector, accept_docs)
  }

  fn default_check_integrity(&self) -> Result<()> {
    self.ensure_open()?;

    // terms/postings
    if let Some(v) = self.get_postings_reader()? {
      v.check_integrity()?;
    }
    // norms
    if let Some(v) = self.get_norms_reader()? {
      v.check_integrity()?;
    }
    // docvalues
    if let Some(v) = self.get_doc_values_reader()? {
      v.check_integrity()?;
    }

    // stored fields
    if let Some(v) = self.get_fields_reader()? {
      v.check_integrity()?;
    }
    // term vectors
    if let Some(v) = self.get_term_vectors_reader()? {
      v.check_integrity()?;
    }

    // points
    if let Some(v) = self.get_points_reader()? {
      v.check_integrity()?;
    }

    // vectors
    if let Some(v) = self.get_vector_reader()? {
      v.check_integrity()?;
    }
    Ok(())
  }
  fn default_do_close(&self) -> Result<()> {
    Ok(())
  }
}
pub type CRFieldsProducer<CR> = <CR as CodecReader>::FieldsProducer;
pub type CRDocValuesProducer<CR> = <CR as CodecReader>::DocValuesProducer;
pub type CRNormsProducer<CR> = <CR as CodecReader>::NormsProducer;
pub type CRPointsReader<CR> = <CR as CodecReader>::PointsReader;
pub type CRKnnVectorReader<CR> = <CR as CodecReader>::KnnVectorsReader;
pub type CRTermVectorsReader<CR> = <CR as CodecReader>::TermVectorsReader;
pub type CRStoredFieldsReader<CR> = <CR as CodecReader>::StoredFieldsReader;
pub type CRBits<CR> = <CR as LeafReader>::Bits;

pub type StoredFieldsType<SF> = StoredFieldsImpl<SF>;
pub type TermVectorsType<TVR> = TermVectorsEnum2<EmptyTermVectors, TVR>;

pub struct StoredFieldsImpl<SF> {
  reader: SF,
  max_doc: i32,
}
impl<SF> StoredFields for StoredFieldsImpl<SF>
where
  SF: StoredFields,
{
  fn prefetch(&mut self, doc_id: i32) -> Result<()> {
    // Don't trust the codec to do proper checks
    CoreHelper::check_index(doc_id, self.max_doc)?;
    self.reader.prefetch(doc_id)
  }

  fn document_with_visitor<S>(
    &mut self,
    doc_id: i32,
    visitor: &mut impl StoredFieldVisitor,
    writer: Option<&mut S>,
  ) -> Result<()>
  where
    S: StoredFieldsWriter,
  {
    // Don't trust the codec to do proper checks
    CoreHelper::check_index(doc_id, self.max_doc)?;
    self.reader.document_with_visitor(doc_id, visitor, writer)
  }
}

impl<SF> RawStoredFieldsReader for StoredFieldsImpl<SF>
where
  SF: RawStoredFieldsReader + StoredFields,
{
  type IndexInput = SF::IndexInput;

  fn raw_stored_fields_mut(&mut self) -> Result<&mut DefaultStoredFieldsReader<Self::IndexInput>> {
    self.reader.raw_stored_fields_mut()
  }

  fn raw_stored_fields(&self) -> Result<&DefaultStoredFieldsReader<Self::IndexInput>> {
    self.reader.raw_stored_fields()
  }
}

macro_rules! either_codec_reader {
    ($vis:vis $name:ident<$($G:ident),+> where [$($base:tt)*] { A: $A:ty, B: $B:ty $(,)? }) => {
        $vis enum $name<$($G),+>
        where
            $($base)*
        {
            A($A),
            B($B),
        }

        impl<$($G),+> Clone for $name<$($G),+>
        where
            $($base)*
            $A: Clone,
            $B: Clone,
        {
            fn clone(&self) -> Self {
                match self {
                    Self::A(inner) => Self::A(inner.clone()),
                    Self::B(inner) => Self::B(inner.clone()),
                }
            }
        }

        impl<$($G),+> Display for $name<$($G),+>
        where
            $($base)*
            $A: Display,
            $B: Display,
        {
            fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
                match self {
                    Self::A(inner) => write!(f, "{}", inner),
                    Self::B(inner) => write!(f, "{}", inner),
                }
            }
        }

        impl<$($G),+> IndexReader for $name<$($G),+>
        where
            $($base)*
            $A: LeafReader,
            $B: LeafReader,
        {
            type ContextKind = LeafReaderContextKind;

            type TermVectors =
                TermVectorsEnum2<<$A as IndexReader>::TermVectors, <$B as IndexReader>::TermVectors>;

            fn term_vectors(&self) -> Result<Self::TermVectors> {
                match self {
                    Self::A(inner) => Ok(TermVectorsEnum2::A(IndexReader::term_vectors(inner)?)),
                    Self::B(inner) => Ok(TermVectorsEnum2::B(IndexReader::term_vectors(inner)?)),
                }
            }

            fn max_doc(&self) -> Result<i32> {
                match self {
                    Self::A(inner) => inner.max_doc(),
                    Self::B(inner) => inner.max_doc(),
                }
            }

            fn num_docs(&self) -> Result<i32> {
                match self {
                    Self::A(inner) => inner.num_docs(),
                    Self::B(inner) => inner.num_docs(),
                }
            }

            type StoredFields =
                StoredFieldsEnum2<<$A as IndexReader>::StoredFields, <$B as IndexReader>::StoredFields>;

            fn stored_fields(&self) -> Result<Self::StoredFields> {
                match self {
                    Self::A(inner) => Ok(StoredFieldsEnum2::A(IndexReader::stored_fields(inner)?)),
                    Self::B(inner) => Ok(StoredFieldsEnum2::B(IndexReader::stored_fields(inner)?)),
                }
            }

            type ReaderCacheHelper = CacheHelperEnum2<
                <$A as IndexReader>::ReaderCacheHelper,
                <$B as IndexReader>::ReaderCacheHelper,
            >;

            fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
                match self {
                    Self::A(inner) => Ok(inner
                        .get_reader_cache_helper()?
                        .map(CacheHelperEnum2::A)),
                    Self::B(inner) => Ok(inner
                        .get_reader_cache_helper()?
                        .map(CacheHelperEnum2::B)),
                }
            }

            fn doc_freq(&self, term: &crate::core::index::term::Term) -> Result<i32> {
                match self {
                    Self::A(inner) => IndexReader::doc_freq(inner,term),
                    Self::B(inner) => IndexReader::doc_freq(inner,term),
                }
            }

            fn total_term_freq(&self, term: &crate::core::index::term::Term) -> Result<i64> {
                match self {
                    Self::A(inner) => inner.total_term_freq(term),
                    Self::B(inner) => inner.total_term_freq(term),
                }
            }

            fn get_sum_doc_freq(&self, field: &str) -> Result<i64> {
                match self {
                    Self::A(inner) => IndexReader::get_sum_doc_freq(inner,field),
                    Self::B(inner) => IndexReader::get_sum_doc_freq(inner,field),
                }
            }

            fn get_doc_count(&self, field: &str) -> Result<i32> {
                match self {
                    Self::A(inner) => IndexReader::get_doc_count(inner,field),
                    Self::B(inner) => IndexReader::get_doc_count(inner,field),
                }
            }

            fn get_sum_total_term_freq(&self, field: &str) -> Result<i64> {
                match self {
                    Self::A(inner) => IndexReader::get_sum_total_term_freq(inner,field),
                    Self::B(inner) => IndexReader::get_sum_total_term_freq(inner,field),
                }
            }

            fn index_base(&self) -> &crate::core::index::index_reader::IndexReaderBase {
                match self {
                    Self::A(inner) => inner.index_base(),
                    Self::B(inner) => inner.index_base(),
                }
            }
        }

        impl<$($G),+> LeafReader for $name<$($G),+>
        where
            $($base)*
            $A: LeafReader,
            $B: LeafReader,
        {
            type CacheHelper = CacheHelperEnum2<
                <$A as LeafReader>::CacheHelper,
                <$B as LeafReader>::CacheHelper,
            >;

            fn get_core_cache_helper(&self) -> Result<Option<Self::CacheHelper>> {
                match self {
                    Self::A(inner) => Ok(inner
                        .get_core_cache_helper()?
                        .map(CacheHelperEnum2::A)),
                    Self::B(inner) => Ok(inner
                        .get_core_cache_helper()?
                        .map(CacheHelperEnum2::B)),
                }
            }

            type Terms = TermsEnum2<<$A as LeafReader>::Terms, <$B as LeafReader>::Terms>;

            fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
                match self {
                    Self::A(inner) => LeafReader::terms(inner, field).map(|opt| opt.map(TermsEnum2::A)),
                    Self::B(inner) => LeafReader::terms(inner, field).map(|opt| opt.map(TermsEnum2::B)),
                }
            }

            type NumericDocValues =
                NumericDocValuesEnum2<<$A as LeafReader>::NumericDocValues, <$B as LeafReader>::NumericDocValues>;

            fn get_numeric_doc_values(&self, field: &str) -> Result<Option<Self::NumericDocValues>> {
                match self {
                    Self::A(inner) => LeafReader::get_numeric_doc_values(inner,field).map(|opt| opt.map(NumericDocValuesEnum2::A)),
                    Self::B(inner) => LeafReader::get_numeric_doc_values(inner,field).map(|opt| opt.map(NumericDocValuesEnum2::B)),
                }
            }

            type BinaryDocValues =
                BinaryDocValuesEnum2<<$A as LeafReader>::BinaryDocValues, <$B as LeafReader>::BinaryDocValues>;

            fn get_binary_doc_values(&self, field: &str) -> Result<Option<Self::BinaryDocValues>> {
                match self {
                    Self::A(inner) => LeafReader::get_binary_doc_values(inner,field).map(|opt| opt.map(BinaryDocValuesEnum2::A)),
                    Self::B(inner) => LeafReader::get_binary_doc_values(inner,field).map(|opt| opt.map(BinaryDocValuesEnum2::B)),
                }
            }

            type SortedDocValues =
                SortedDocValuesEnum2<<$A as LeafReader>::SortedDocValues, <$B as LeafReader>::SortedDocValues>;

            fn get_sorted_doc_values(&self, field: &str) -> Result<Option<Self::SortedDocValues>> {
                match self {
                    Self::A(inner) => LeafReader::get_sorted_doc_values(inner,field).map(|opt| opt.map(SortedDocValuesEnum2::A)),
                    Self::B(inner) => LeafReader::get_sorted_doc_values(inner,field).map(|opt| opt.map(SortedDocValuesEnum2::B)),
                }
            }

            type SortedNumericDocValues =
                SortedNumericDocValuesEnum2<<$A as LeafReader>::SortedNumericDocValues, <$B as LeafReader>::SortedNumericDocValues>;

            fn get_sorted_numeric_doc_values(
                &self,
                field: &str,
            ) -> Result<Option<Self::SortedNumericDocValues>> {
                match self {
                    Self::A(inner) =>LeafReader::
                        get_sorted_numeric_doc_values(inner,field)
                        .map(|opt| opt.map(SortedNumericDocValuesEnum2::A)),
                    Self::B(inner) => LeafReader::
                        get_sorted_numeric_doc_values(inner,field)
                        .map(|opt| opt.map(SortedNumericDocValuesEnum2::B)),
                }
            }

            type SortedSetDocValues =
                SortedSetDocValuesEnum2<<$A as LeafReader>::SortedSetDocValues, <$B as LeafReader>::SortedSetDocValues>;

            fn get_sorted_set_doc_values(
                &self,
                field: &str,
            ) -> Result<Option<Self::SortedSetDocValues>> {
                match self {
                    Self::A(inner) => LeafReader::
                        get_sorted_set_doc_values(inner, field)
                        .map(|opt| opt.map(SortedSetDocValuesEnum2::A)),
                    Self::B(inner) =>LeafReader::
                        get_sorted_set_doc_values(inner,field)
                        .map(|opt| opt.map(SortedSetDocValuesEnum2::B)),
                }
            }

            type NormNumericDocValues =
                NumericDocValuesEnum2<<$A as LeafReader>::NormNumericDocValues, <$B as LeafReader>::NormNumericDocValues>;

            fn get_norm_values(&self, field: &str) -> Result<Option<Self::NormNumericDocValues>> {
                match self {
                    Self::A(inner) => LeafReader::get_norm_values(inner,field).map(|opt| opt.map(NumericDocValuesEnum2::A)),
                    Self::B(inner) => LeafReader::get_norm_values(inner,field).map(|opt| opt.map(NumericDocValuesEnum2::B)),
                }
            }

            type DocValuesSkipper =
                DocValuesSkipperEnum2<<$A as LeafReader>::DocValuesSkipper, <$B as LeafReader>::DocValuesSkipper>;

            fn get_doc_values_skipper(
                &self,
                field: &str,
            ) -> Result<Option<Self::DocValuesSkipper>> {
                match self {
                    Self::A(inner) => LeafReader::get_doc_values_skipper(inner,field).map(|opt| opt.map(DocValuesSkipperEnum2::A)),
                    Self::B(inner) => LeafReader::get_doc_values_skipper(inner,field).map(|opt| opt.map(DocValuesSkipperEnum2::B)),
                }
            }

            type FloatVectorValues = FloatVectorValuesEnum2<<$A as LeafReader>::FloatVectorValues, <$B as LeafReader>::FloatVectorValues>;

            fn get_float_vector_values(&self, field: &str) -> Result<Option<Self::FloatVectorValues>> {
                match self {
                    Self::A(inner) => LeafReader::get_float_vector_values(inner,field).map(|opt| opt.map(FloatVectorValuesEnum2::A)),
                    Self::B(inner) => LeafReader::get_float_vector_values(inner,field).map(|opt| opt.map(FloatVectorValuesEnum2::B)),
                }
            }

           type ByteVectorValues = ByteVectorValuesEnum2<<$A as LeafReader>::ByteVectorValues, <$B as LeafReader>::ByteVectorValues>;

            fn get_byte_vector_values(&self, field: &str) -> Result<Option<Self::ByteVectorValues>> {
                match self {
                    Self::A(inner) => LeafReader::get_byte_vector_values(inner,field).map(|opt| opt.map(ByteVectorValuesEnum2::A)),
                    Self::B(inner) => LeafReader::get_byte_vector_values(inner,field).map(|opt| opt.map(ByteVectorValuesEnum2::B)),
                }
            }

            fn search_nearest_vectors_f32<BitsT, K>(
                &self,
                field: &str,
                target: Vec<f32>,
                knn_collector: &mut K,
                accept_docs: Option<BitsT>,
            ) -> Result<()>
            where
                BitsT: crate::core::util::bits::Bits,
                K: KnnCollector,
            {
                match self {
                    Self::A(inner) => LeafReader::search_nearest_vectors_f32(inner,field,target,knn_collector,accept_docs),
                    Self::B(inner) => LeafReader::search_nearest_vectors_f32(inner,field,target,knn_collector,accept_docs),
                }
            }

            fn search_nearest_vectors_u8<BitsT, K>(
                &self,
                field: &str,
                target: Vec<u8>,
                knn_collector: &mut K,
                accept_docs: Option<BitsT>,
            ) -> Result<()>
            where
                BitsT: crate::core::util::bits::Bits,
                K: KnnCollector,
            {
                match self {
                    Self::A(inner) => LeafReader::search_nearest_vectors_u8(inner,field,target,knn_collector,accept_docs),
                    Self::B(inner) => LeafReader::search_nearest_vectors_u8(inner,field,target,knn_collector,accept_docs),
                }
            }


            fn get_field_infos(&self) -> Result<Arc<crate::core::index::field_infos::FieldInfos>> {
                match self {
                    Self::A(inner) => inner.get_field_infos(),
                    Self::B(inner) => inner.get_field_infos(),
                }
            }

            type Bits = BitsEnum2<<$A as LeafReader>::Bits, <$B as LeafReader>::Bits>;

            fn get_live_docs(&self) -> Result<Option<Self::Bits>> {
                match self {
                    Self::A(inner) => inner.get_live_docs().map(|opt| opt.map(BitsEnum2::A)),
                    Self::B(inner) => inner.get_live_docs().map(|opt| opt.map(BitsEnum2::B)),
                }
            }

            type PointValues =
                PointValuesEnum2<<$A as LeafReader>::PointValues, <$B as LeafReader>::PointValues>;

            fn get_point_values(&self, field: &str) -> Result<Option<Self::PointValues>> {
                match self {
                    Self::A(inner) => LeafReader::get_point_values(inner,field).map(|opt| opt.map(PointValuesEnum2::A)),
                    Self::B(inner) => LeafReader::get_point_values(inner,field).map(|opt| opt.map(PointValuesEnum2::B)),
                }
            }

            fn get_metadata(&self) -> Result<&crate::core::index::leaf_metadata::LeafMetaData> {
                match self {
                    Self::A(inner) => inner.get_metadata(),
                    Self::B(inner) => inner.get_metadata(),
                }
            }
        }

        impl<$($G),+> CodecReader for $name<$($G),+>
        where
            $($base)*
            $A: CodecReader,
            $B: CodecReader,
            <$B as CodecReader>::StoredFieldsReader: RawStoredFieldsReader<
                IndexInput = <<$A as CodecReader>::StoredFieldsReader as RawStoredFieldsReader>::IndexInput,
            >,
            <$B as CodecReader>::TermVectorsReader: RawTermVectors<
                IndexInput = <<$A as CodecReader>::TermVectorsReader as RawTermVectors>::IndexInput,
            >,
        {
            type StoredFieldsReader =
                StoredFieldsReaderEnum2<<$A as CodecReader>::StoredFieldsReader, <$B as CodecReader>::StoredFieldsReader>;
            type TermVectorsReader =
                TermVectorsReaderEnum2<<$A as CodecReader>::TermVectorsReader, <$B as CodecReader>::TermVectorsReader>;
            type NormsProducer =
                NormsProducerEnum2<<$A as CodecReader>::NormsProducer, <$B as CodecReader>::NormsProducer>;
            type DocValuesProducer =
                DocValuesProducerEnum2<<$A as CodecReader>::DocValuesProducer, <$B as CodecReader>::DocValuesProducer>;
            type FieldsProducer =
                FieldsProducerEnum2<<$A as CodecReader>::FieldsProducer, <$B as CodecReader>::FieldsProducer>;
            type PointsReader =
                PointsReaderEnum2<<$A as CodecReader>::PointsReader, <$B as CodecReader>::PointsReader>;
            type KnnVectorsReader = KnnVectorsReaderEnum2<<$A as CodecReader>::KnnVectorsReader, <$B as CodecReader>::KnnVectorsReader>;

            fn get_fields_reader(&self) -> Result<Option<Self::StoredFieldsReader>> {
                match self {
                    Self::A(inner) => inner
                        .get_fields_reader()
                        .map(|opt| opt.map(StoredFieldsReaderEnum2::A)),
                    Self::B(inner) => inner
                        .get_fields_reader()
                        .map(|opt| opt.map(StoredFieldsReaderEnum2::B)),
                }
            }

            fn get_term_vectors_reader(&self) -> Result<Option<Self::TermVectorsReader>> {
                match self {
                    Self::A(inner) => inner
                        .get_term_vectors_reader()
                        .map(|opt| opt.map(TermVectorsReaderEnum2::A)),
                    Self::B(inner) => inner
                        .get_term_vectors_reader()
                        .map(|opt| opt.map(TermVectorsReaderEnum2::B)),
                }
            }

            fn get_norms_reader(&self) -> Result<Option<Self::NormsProducer>> {
                match self {
                    Self::A(inner) => inner
                        .get_norms_reader()
                        .map(|opt| opt.map(NormsProducerEnum2::A)),
                    Self::B(inner) => inner
                        .get_norms_reader()
                        .map(|opt| opt.map(NormsProducerEnum2::B)),
                }
            }

            fn get_doc_values_reader(&self) -> Result<Option<Self::DocValuesProducer>> {
                match self {
                    Self::A(inner) => inner
                        .get_doc_values_reader()
                        .map(|opt| opt.map(DocValuesProducerEnum2::A)),
                    Self::B(inner) => inner
                        .get_doc_values_reader()
                        .map(|opt| opt.map(DocValuesProducerEnum2::B)),
                }
            }

            fn get_postings_reader(&self) -> Result<Option<Self::FieldsProducer>> {
                match self {
                    Self::A(inner) => inner
                        .get_postings_reader()
                        .map(|opt| opt.map(FieldsProducerEnum2::A)),
                    Self::B(inner) => inner
                        .get_postings_reader()
                        .map(|opt| opt.map(FieldsProducerEnum2::B)),
                }
            }

            fn get_points_reader(&self) -> Result<Option<Self::PointsReader>> {
                match self {
                    Self::A(inner) => inner
                        .get_points_reader()
                        .map(|opt| opt.map(PointsReaderEnum2::A)),
                    Self::B(inner) => inner
                        .get_points_reader()
                        .map(|opt| opt.map(PointsReaderEnum2::B)),
                }
            }

            fn get_vector_reader(&self) -> Result<Option<Self::KnnVectorsReader>> {
                match self {
                    Self::A(inner) => inner
                    .get_vector_reader()
                    .map(|opt| opt.map(KnnVectorsReaderEnum2::A)),
                    Self::B(inner) => inner
                    .get_vector_reader()
                    .map(|opt| opt.map(KnnVectorsReaderEnum2::B)),
                }
            }
        }
    };
}

either_codec_reader!(pub CodecReaderEnum2<A, B> where [] { A: A, B: B });

either_codec_reader!(
    pub(crate) SlowCompositeCodecReader<CR>
    where [CR: CodecReader + Clone,]
    {
        A: CR,
        B: crate::core::index::slow_composite_codec_reader_wrapper::SlowCompositeCodecReaderWrapper<CR>,
    }
);

pub enum SoftDeletesCodecReader<CR>
where
  CR: CodecReader,
  CR::ReaderCacheHelper: Clone,
{
  A(CR),
  B(crate::core::index::soft_deletes_directory_reader_wrapper::SoftDeletesFilterCodecReader<CR>),
}

impl<CR> Clone for SoftDeletesCodecReader<CR>
where
  CR: CodecReader + Clone,
  CR::ReaderCacheHelper: Clone,
{
  fn clone(&self) -> Self {
    match self {
      Self::A(inner) => Self::A(inner.clone()),
      Self::B(inner) => Self::B(inner.clone()),
    }
  }
}

impl<CR> Display for SoftDeletesCodecReader<CR>
where
  CR: CodecReader,
  CR::ReaderCacheHelper: Clone,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::A(inner) => write!(f, "{}", inner),
      Self::B(inner) => write!(f, "{}", inner),
    }
  }
}

impl<CR> IndexReader for SoftDeletesCodecReader<CR>
where
  CR: CodecReader,
  CR::ReaderCacheHelper: Clone,
{
  type ContextKind = LeafReaderContextKind;
  type TermVectors = CR::TermVectors;

  fn term_vectors(&self) -> Result<Self::TermVectors> {
    match self {
      Self::A(inner) => IndexReader::term_vectors(inner),
      Self::B(inner) => IndexReader::term_vectors(inner),
    }
  }

  fn max_doc(&self) -> Result<i32> {
    match self {
      Self::A(inner) => inner.max_doc(),
      Self::B(inner) => inner.max_doc(),
    }
  }

  fn num_docs(&self) -> Result<i32> {
    match self {
      Self::A(inner) => inner.num_docs(),
      Self::B(inner) => inner.num_docs(),
    }
  }

  type StoredFields = CR::StoredFields;

  fn stored_fields(&self) -> Result<Self::StoredFields> {
    match self {
      Self::A(inner) => IndexReader::stored_fields(inner),
      Self::B(inner) => IndexReader::stored_fields(inner),
    }
  }

  type ReaderCacheHelper =
    CacheHelperEnum2<CR::ReaderCacheHelper, DelegatingCacheHelper<CR::ReaderCacheHelper>>;

  fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
    match self {
      Self::A(inner) => Ok(inner.get_reader_cache_helper()?.map(CacheHelperEnum2::A)),
      Self::B(inner) => Ok(inner.get_reader_cache_helper()?.map(CacheHelperEnum2::B)),
    }
  }

  fn doc_freq(&self, term: &crate::core::index::term::Term) -> Result<i32> {
    match self {
      Self::A(inner) => IndexReader::doc_freq(inner, term),
      Self::B(inner) => IndexReader::doc_freq(inner, term),
    }
  }

  fn total_term_freq(&self, term: &crate::core::index::term::Term) -> Result<i64> {
    match self {
      Self::A(inner) => inner.total_term_freq(term),
      Self::B(inner) => inner.total_term_freq(term),
    }
  }

  fn get_sum_doc_freq(&self, field: &str) -> Result<i64> {
    match self {
      Self::A(inner) => IndexReader::get_sum_doc_freq(inner, field),
      Self::B(inner) => IndexReader::get_sum_doc_freq(inner, field),
    }
  }

  fn get_doc_count(&self, field: &str) -> Result<i32> {
    match self {
      Self::A(inner) => IndexReader::get_doc_count(inner, field),
      Self::B(inner) => IndexReader::get_doc_count(inner, field),
    }
  }

  fn get_sum_total_term_freq(&self, field: &str) -> Result<i64> {
    match self {
      Self::A(inner) => IndexReader::get_sum_total_term_freq(inner, field),
      Self::B(inner) => IndexReader::get_sum_total_term_freq(inner, field),
    }
  }

  fn index_base(&self) -> &crate::core::index::index_reader::IndexReaderBase {
    match self {
      Self::A(inner) => inner.index_base(),
      Self::B(inner) => inner.index_base(),
    }
  }
}

impl<CR> LeafReader for SoftDeletesCodecReader<CR>
where
  CR: CodecReader,
  CR::ReaderCacheHelper: Clone,
{
  type CacheHelper = CR::CacheHelper;

  fn get_core_cache_helper(&self) -> Result<Option<Self::CacheHelper>> {
    match self {
      Self::A(inner) => inner.get_core_cache_helper(),
      Self::B(inner) => inner.get_core_cache_helper(),
    }
  }

  type Terms = CR::Terms;

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    match self {
      Self::A(inner) => LeafReader::terms(inner, field),
      Self::B(inner) => LeafReader::terms(inner, field),
    }
  }

  type NumericDocValues = CR::NumericDocValues;

  fn get_numeric_doc_values(&self, field: &str) -> Result<Option<Self::NumericDocValues>> {
    match self {
      Self::A(inner) => LeafReader::get_numeric_doc_values(inner, field),
      Self::B(inner) => LeafReader::get_numeric_doc_values(inner, field),
    }
  }

  type BinaryDocValues = CR::BinaryDocValues;

  fn get_binary_doc_values(&self, field: &str) -> Result<Option<Self::BinaryDocValues>> {
    match self {
      Self::A(inner) => LeafReader::get_binary_doc_values(inner, field),
      Self::B(inner) => LeafReader::get_binary_doc_values(inner, field),
    }
  }

  type SortedDocValues = CR::SortedDocValues;

  fn get_sorted_doc_values(&self, field: &str) -> Result<Option<Self::SortedDocValues>> {
    match self {
      Self::A(inner) => LeafReader::get_sorted_doc_values(inner, field),
      Self::B(inner) => LeafReader::get_sorted_doc_values(inner, field),
    }
  }

  type SortedNumericDocValues = CR::SortedNumericDocValues;

  fn get_sorted_numeric_doc_values(
    &self,
    field: &str,
  ) -> Result<Option<Self::SortedNumericDocValues>> {
    match self {
      Self::A(inner) => LeafReader::get_sorted_numeric_doc_values(inner, field),
      Self::B(inner) => LeafReader::get_sorted_numeric_doc_values(inner, field),
    }
  }

  type SortedSetDocValues = CR::SortedSetDocValues;

  fn get_sorted_set_doc_values(&self, field: &str) -> Result<Option<Self::SortedSetDocValues>> {
    match self {
      Self::A(inner) => LeafReader::get_sorted_set_doc_values(inner, field),
      Self::B(inner) => LeafReader::get_sorted_set_doc_values(inner, field),
    }
  }

  type NormNumericDocValues = CR::NormNumericDocValues;

  fn get_norm_values(&self, field: &str) -> Result<Option<Self::NormNumericDocValues>> {
    match self {
      Self::A(inner) => LeafReader::get_norm_values(inner, field),
      Self::B(inner) => LeafReader::get_norm_values(inner, field),
    }
  }

  type DocValuesSkipper = CR::DocValuesSkipper;

  fn get_doc_values_skipper(&self, field: &str) -> Result<Option<Self::DocValuesSkipper>> {
    match self {
      Self::A(inner) => LeafReader::get_doc_values_skipper(inner, field),
      Self::B(inner) => LeafReader::get_doc_values_skipper(inner, field),
    }
  }

  type FloatVectorValues = CR::FloatVectorValues;

  fn get_float_vector_values(&self, field: &str) -> Result<Option<Self::FloatVectorValues>> {
    match self {
      Self::A(inner) => LeafReader::get_float_vector_values(inner, field),
      Self::B(inner) => LeafReader::get_float_vector_values(inner, field),
    }
  }

  type ByteVectorValues = CR::ByteVectorValues;

  fn get_byte_vector_values(&self, field: &str) -> Result<Option<Self::ByteVectorValues>> {
    match self {
      Self::A(inner) => LeafReader::get_byte_vector_values(inner, field),
      Self::B(inner) => LeafReader::get_byte_vector_values(inner, field),
    }
  }

  fn search_nearest_vectors_f32<B, K>(
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
      Self::A(inner) => {
        LeafReader::search_nearest_vectors_f32(inner, field, target, knn_collector, accept_docs)
      },
      Self::B(inner) => {
        LeafReader::search_nearest_vectors_f32(inner, field, target, knn_collector, accept_docs)
      },
    }
  }

  fn search_nearest_vectors_u8<B, K>(
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
      Self::A(inner) => {
        LeafReader::search_nearest_vectors_u8(inner, field, target, knn_collector, accept_docs)
      },
      Self::B(inner) => {
        LeafReader::search_nearest_vectors_u8(inner, field, target, knn_collector, accept_docs)
      },
    }
  }

  fn get_field_infos(&self) -> Result<Arc<crate::core::index::field_infos::FieldInfos>> {
    match self {
      Self::A(inner) => inner.get_field_infos(),
      Self::B(inner) => inner.get_field_infos(),
    }
  }

  type Bits = BitsEnum2<CR::Bits, Arc<FixedBitSet>>;

  fn get_live_docs(&self) -> Result<Option<Self::Bits>> {
    match self {
      Self::A(inner) => inner.get_live_docs().map(|opt| opt.map(BitsEnum2::A)),
      Self::B(inner) => inner.get_live_docs().map(|opt| opt.map(BitsEnum2::B)),
    }
  }

  type PointValues = CR::PointValues;

  fn get_point_values(&self, field: &str) -> Result<Option<Self::PointValues>> {
    match self {
      Self::A(inner) => LeafReader::get_point_values(inner, field),
      Self::B(inner) => LeafReader::get_point_values(inner, field),
    }
  }

  fn get_metadata(&self) -> Result<&crate::core::index::leaf_metadata::LeafMetaData> {
    match self {
      Self::A(inner) => inner.get_metadata(),
      Self::B(inner) => inner.get_metadata(),
    }
  }
}

impl<CR> CodecReader for SoftDeletesCodecReader<CR>
where
  CR: CodecReader,
  CR::ReaderCacheHelper: Clone,
{
  type StoredFieldsReader = CR::StoredFieldsReader;
  type TermVectorsReader = CR::TermVectorsReader;
  type NormsProducer = CR::NormsProducer;
  type DocValuesProducer = CR::DocValuesProducer;
  type FieldsProducer = CR::FieldsProducer;
  type PointsReader = CR::PointsReader;
  type KnnVectorsReader = CR::KnnVectorsReader;

  fn get_fields_reader(&self) -> Result<Option<Self::StoredFieldsReader>> {
    match self {
      Self::A(inner) => inner.get_fields_reader(),
      Self::B(inner) => inner.get_fields_reader(),
    }
  }

  fn get_term_vectors_reader(&self) -> Result<Option<Self::TermVectorsReader>> {
    match self {
      Self::A(inner) => inner.get_term_vectors_reader(),
      Self::B(inner) => inner.get_term_vectors_reader(),
    }
  }

  fn get_norms_reader(&self) -> Result<Option<Self::NormsProducer>> {
    match self {
      Self::A(inner) => inner.get_norms_reader(),
      Self::B(inner) => inner.get_norms_reader(),
    }
  }

  fn get_doc_values_reader(&self) -> Result<Option<Self::DocValuesProducer>> {
    match self {
      Self::A(inner) => inner.get_doc_values_reader(),
      Self::B(inner) => inner.get_doc_values_reader(),
    }
  }

  fn get_postings_reader(&self) -> Result<Option<Self::FieldsProducer>> {
    match self {
      Self::A(inner) => inner.get_postings_reader(),
      Self::B(inner) => inner.get_postings_reader(),
    }
  }

  fn get_points_reader(&self) -> Result<Option<Self::PointsReader>> {
    match self {
      Self::A(inner) => inner.get_points_reader(),
      Self::B(inner) => inner.get_points_reader(),
    }
  }

  fn get_vector_reader(&self) -> Result<Option<Self::KnnVectorsReader>> {
    match self {
      Self::A(inner) => inner.get_vector_reader(),
      Self::B(inner) => inner.get_vector_reader(),
    }
  }
}

impl<CR> CodecReader for Arc<CR>
where
  CR: CodecReader,
{
  type StoredFieldsReader = CR::StoredFieldsReader;
  type TermVectorsReader = CR::TermVectorsReader;
  type NormsProducer = CR::NormsProducer;
  type DocValuesProducer = CR::DocValuesProducer;
  type FieldsProducer = CR::FieldsProducer;
  type PointsReader = CR::PointsReader;
  type KnnVectorsReader = CR::KnnVectorsReader;

  fn get_fields_reader(&self) -> Result<Option<Self::StoredFieldsReader>> {
    (**self).get_fields_reader()
  }

  fn get_term_vectors_reader(&self) -> Result<Option<Self::TermVectorsReader>> {
    (**self).get_term_vectors_reader()
  }

  fn get_norms_reader(&self) -> Result<Option<Self::NormsProducer>> {
    (**self).get_norms_reader()
  }

  fn get_doc_values_reader(&self) -> Result<Option<Self::DocValuesProducer>> {
    (**self).get_doc_values_reader()
  }

  fn get_postings_reader(&self) -> Result<Option<Self::FieldsProducer>> {
    (**self).get_postings_reader()
  }

  fn get_points_reader(&self) -> Result<Option<Self::PointsReader>> {
    (**self).get_points_reader()
  }

  fn get_vector_reader(&self) -> Result<Option<Self::KnnVectorsReader>> {
    (**self).get_vector_reader()
  }
}
