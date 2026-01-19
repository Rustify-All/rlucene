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
use crate::core::codecs::norms_producer::{NormsProducer, NormsProducerEnum2};
use crate::core::codecs::points_reader::{PointsReader, PointsReaderEnum2};
use crate::core::codecs::stored_fields_reader::{
    DefaultStoredFieldsReader, StoredFieldsReader, StoredFieldsReaderEnum2,
};
use crate::core::codecs::stored_fields_writer::StoredFieldsWriter;
use crate::core::codecs::term_vectors_reader::{TermVectorsReader, TermVectorsReaderEnum2};
use crate::core::index::binary_doc_values::BinaryDocValuesEnum2;
use crate::core::index::doc_values_skip_index_type::DocValuesSkipIndexType;
use crate::core::index::doc_values_skipper::DocValuesSkipperEnum2;
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::dummy::dummy_cache_helper::DummyCacheHelper;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::fields::Fields;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::index_reader::IndexReader;
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
use crate::core::util::bits::BitsEnum2;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::{CoreHelper, TryIntoInt};
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
            None => Ok(TermVectorsEnum2::A(EmptyTermVectors::default())),
        }
    }
    fn terms(
        &self,
        field: &str,
    ) -> Result<Option<<<Self as CodecReader>::FieldsProducer as Fields>::Terms>> {
        self.ensure_open()?;
        let fi = self.get_field_infos()?.field_info_by_name(field);

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
        let fi = self.get_field_infos()?.field_info_by_name(field);

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
    ) -> Result<Option<<Self::DocValuesProducer as DocValuesProducer>::SortedNumericDocValues>>
    {
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

        let fi = self.get_field_infos()?.field_info_by_name(field);
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

        let fi = self.get_field_infos()?.field_info_by_name(field);
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

        let fi = self.get_field_infos()?.field_info_by_name(field);
        match fi {
            Some(f) if f.get_point_dimension_count() > 0 => f,
            _ => return Ok(None),
        };
        let reader = self
            .get_points_reader()?
            .ok_or_else(|| LuceneError::illegal_state("points reader is None"))?;

        reader.get_values(field)
    }

    fn default_check_integrity(&self) -> Result<()> {
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
        // self.get_vector_reader()?.check_integrity()
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
pub type CRTermVectorsReader<CR> = <CR as CodecReader>::TermVectorsReader;
pub type CRStoredFieldsReader<CR> = <CR as CodecReader>::StoredFieldsReader;
pub type CRBits<CR> = <CR as LeafReader>::Bits;

pub type StoredFieldsType<SF> = StoredFieldsImpl<SF>;
pub type TermVectorsType<TVR> =
    TermVectorsEnum2<EmptyTermVectors<<TVR as RawTermVectors>::IndexInput>, TVR>;

pub struct StoredFieldsImpl<SF>
where
    SF: StoredFields,
{
    reader: SF,
    max_doc: i32,
}
impl<SF> StoredFields for StoredFieldsImpl<SF>
where
    SF: StoredFields,
{
    fn prefetch(&mut self, doc_id: i32) -> Result<()> {
        // Don't trust the codec to do proper checks
        CoreHelper::check_index(doc_id.try_convert()?, self.max_doc.try_convert()?)?;
        self.reader.prefetch(doc_id)
    }

    fn document_with_visitor<S: StoredFieldsWriter>(
        &mut self,
        doc_id: i32,
        visitor: &mut impl StoredFieldVisitor,
        writer: Option<&mut S>,
    ) -> Result<()> {
        // Don't trust the codec to do proper checks
        CoreHelper::check_index(doc_id.try_convert()?, self.max_doc.try_convert()?)?;
        self.reader.document_with_visitor(doc_id, visitor, writer)
    }
}

impl<SF> RawStoredFieldsReader for StoredFieldsImpl<SF>
where
    SF: RawStoredFieldsReader + StoredFields,
{
    type IndexInput = SF::IndexInput;

    fn raw_stored_fields(&mut self) -> Result<&mut DefaultStoredFieldsReader<Self::IndexInput>> {
        self.reader.raw_stored_fields()
    }
}

macro_rules! either_codec_reader {
    ($vis:vis $name:ident { A: $A:ident, B: $B:ident $(,)? }) => {
        $vis enum $name<$A, $B> {
            A($A),
            B($B),
        }

        impl<$A, $B> Display for $name<$A, $B>
        where
            $A: CodecReader,
            $B: CodecReader,
        {
            fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
                match self {
                    Self::A(inner) => write!(f, "{}", inner),
                    Self::B(inner) => write!(f, "{}", inner),
                }
            }
        }

        impl<$A, $B> IndexReader for $name<$A, $B>
        where
            $A: CodecReader,
            $B: CodecReader,
        {
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

            type ReaderCacheHelper = DummyCacheHelper;

            fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
                Ok(None)
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

        impl<$A, $B> LeafReader for $name<$A, $B>
        where
            $A: CodecReader,
            $B: CodecReader,
        {
            type CacheHelper = DummyCacheHelper;

            fn get_core_cache_helper_ref(&self) -> Result<Option<&Self::CacheHelper>> {
                Ok(None)
            }

            fn get_core_cache_helper(&self) -> Result<Option<Self::CacheHelper>> {
                Ok(None)
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

        impl<$A, $B> CodecReader for $name<$A, $B>
        where
            $A: CodecReader,
            $B: CodecReader,
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
        }
    };
}

either_codec_reader!(pub CodecReaderEnum2 { A: A, B: B });

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
}
