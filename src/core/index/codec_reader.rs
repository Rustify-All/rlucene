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
use crate::core::codecs::doc_values_producer::DocValuesProducer;
use crate::core::codecs::fields_producer::FieldsProducer;
use crate::core::codecs::norms_producer::NormsProducer;
use crate::core::codecs::points_reader::PointsReader;
use crate::core::codecs::stored_fields_reader::StoredFieldsReader;
use crate::core::codecs::stored_fields_writer::StoredFieldsWriter;
use crate::core::codecs::term_vectors_reader::TermVectorsReader;
use crate::core::index::doc_values_skip_index_type::DocValuesSkipIndexType;
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::fields::Fields;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::stored_field_visitor::StoredFieldVisitor;
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::term_vectors::{Either2TermVectors, EmptyTermVectors};
use crate::core::util::CoreHelper;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::borrow::Cow;
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
    fn get_fields_reader(&self) -> Result<Cow<'_, Self::StoredFieldsReader>>;

    /// Expert: retrieve underlying TermVectorsReader
    fn get_term_vectors_reader(&self) -> Result<Option<Cow<'_, Self::TermVectorsReader>>>;

    /// Expert: retrieve underlying NormsProducer
    fn get_norms_reader(&self) -> Result<Option<Cow<'_, Self::NormsProducer>>>;

    /// Expert: retrieve underlying DocValuesProducer
    fn get_doc_values_reader(&self) -> Result<Option<Cow<'_, Self::DocValuesProducer>>>;

    /// Expert: retrieve underlying FieldsProducer (postings)
    fn get_postings_reader(&self) -> Result<Option<Cow<'_, Self::FieldsProducer>>>;

    /// Expert: retrieve underlying PointsReader
    fn get_points_reader(&self) -> Result<Option<Cow<'_, Self::PointsReader>>>;

    fn stored_fields(&self) -> Result<StoredFieldsImpl<Self::StoredFieldsReader>> {
        let reader = self.get_fields_reader()?;
        debug_assert!(matches!(reader, Cow::Owned(_)));
        Ok(StoredFieldsImpl {
            reader: self.get_fields_reader()?.into_owned(),
            max_doc: self.max_doc()?,
        })
    }

    fn term_vectors(
        &self,
        _field: &str,
    ) -> Result<Either2TermVectors<EmptyTermVectors, Self::TermVectorsReader>> {
        let reader = self.get_term_vectors_reader()?;
        match reader {
            Some(r) => {
                debug_assert!(matches!(r, Cow::Owned(_)));
                Ok(Either2TermVectors::B(r.into_owned()))
            },
            None => Ok(Either2TermVectors::A(EmptyTermVectors {})),
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

        Ok(Some(reader.get_skipper(&fi)?))
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

        Ok(Some(reader.get_values(field)?))
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
        self.get_fields_reader()?.check_integrity()?;

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
        CoreHelper::check_index(doc_id, self.max_doc)?;
        self.reader.prefetch(doc_id)
    }

    fn document_with_visitor(
        &mut self,
        doc_id: i32,
        visitor: &mut impl StoredFieldVisitor,
        writer: &mut impl StoredFieldsWriter,
    ) -> Result<()> {
        // Don't trust the codec to do proper checks
        CoreHelper::check_index(doc_id, self.max_doc)?;
        self.reader.document_with_visitor(doc_id, visitor, writer)
    }
}
