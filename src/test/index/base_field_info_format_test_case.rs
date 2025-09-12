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
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use rand::Rng;
use strum::EnumCount;

use crate::core::codecs::field_infos_format::FieldInfosFormat;
use crate::core::codecs::{Codec, get_default_code};
use crate::core::document::field_type::FieldType;
use crate::core::index::doc_values_skip_index_type::DocValuesSkipIndexType;
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::indexable_field_type::IndexableFieldType;
use crate::core::index::point_values::{MAX_DIMENSIONS, MAX_INDEX_DIMENSIONS, MAX_NUM_BYTES};
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::vector_encoding::VectorEncoding;
use crate::core::index::vector_similarity_function::VectorSimilarityFunction;
use crate::core::store::IOContext;
use crate::core::store::directory::Directory;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::{LATEST, StringHelper};
use crate::test::util::index_package_access::{
    FieldInfosBuilder, IndexPackageAccess, IndexPackageAccessImpl,
};
use crate::test::util::lucene_test_case::lucene_test_case_util::{at_least, new_directory};
use crate::test::util::test_util::TestUtil;

pub trait BaseFieldInfoFormatTestCase {
    fn support_doc_values_skip_index(&self) -> bool {
        true
    }
    fn test_one_field<R: Rng + ?Sized>(&self, random: &mut R) -> Result<()> {
        let dir = Arc::new(new_directory(random)?);
        let codec = get_default_code();
        let segment_info = Self::new_segment_info(random, dir.clone(), "_123")?;

        let fi = Arc::new(Self::create_field_info());
        Self::add_attributes(&fi);

        let infos = IndexPackageAccessImpl
            .new_field_infos_builder(None, None)?
            .add(fi)?
            .finish()?;

        codec.field_infos_format().write(
            dir.as_ref(),
            &segment_info,
            "",
            &infos,
            &IOContext::default_io_context()?,
        )?;
        let infos2 = codec.field_infos_format().read(
            dir.as_ref(),
            &segment_info,
            "",
            &IOContext::default_io_context()?,
        )?;

        assert_eq!(1, infos2.size());
        match infos2.field_info_by_name("field") {
            None => {
                unreachable!("field not found");
            },
            Some(field) => {
                assert_ne!(field.get_index_options(), &IndexOptions::None);
                assert_eq!(DocValuesType::None, *field.get_doc_values_type());
                assert!(!field.omits_norms());
                assert!(!field.has_payloads());
                assert!(!field.has_term_vectors());
                assert_eq!(0, field.get_point_dimension_count());
                assert_eq!(0, field.get_vector_dimension());
                assert!(!field.is_soft_deletes_field());
            },
        }
        Ok(())
    }
    /// Test field infos attributes coming back are not mutable.
    fn test_immutable_attributes<R: Rng + ?Sized>(&self, _random: &mut R) -> Result<()> {
        // no necessary to implement
        Ok(())
    }

    fn test_exception_on_create_output<R: Rng + ?Sized>(&self, _random: &mut R) -> Result<()> {
        // TODO
        // no necessary to implement
        Ok(())
    }
    fn test_exception_on_close_output<R: Rng + ?Sized>(&self, _random: &mut R) -> Result<()> {
        // TODO
        // no necessary to implement
        Ok(())
    }
    fn test_exception_on_open_input<R: Rng + ?Sized>(&self, _random: &mut R) -> Result<()> {
        // TODO
        // no necessary to implement
        Ok(())
    }
    fn test_exception_on_close_input<R: Rng + ?Sized>(&self, _random: &mut R) -> Result<()> {
        // TODO
        // no necessary to implement
        Ok(())
    }
    // Test field infos read/write with random fields, with different values.
    fn test_random<R: Rng + ?Sized>(&self, random: &mut R) -> Result<()> {
        let dir = Arc::new(new_directory(random)?);
        let codec = get_default_code();
        let segment_info = Self::new_segment_info(random, dir.clone(), "_123")?;
        let num_fields = at_least(random, 2000);
        let mut field_names = HashSet::new();
        for _ in 0..num_fields {
            field_names.insert(TestUtil::random_unicode_string(random));
        }
        let soft_deletes_field = if random.random_bool(0.5) {
            Some(TestUtil::random_unicode_string(random))
        } else {
            None
        };

        let mut parent_field = if random.random_bool(0.5) {
            Some(TestUtil::random_unicode_string(random))
        } else {
            None
        };

        // Ensure softDeletesField and parentField are not equal.
        if soft_deletes_field.is_some() && soft_deletes_field == parent_field {
            parent_field = None;
        }

        // Create a new FieldInfos builder.
        let soft_deletes_field_clone = soft_deletes_field.clone();
        let parent_field_clone = parent_field.clone();
        let mut builder =
            IndexPackageAccessImpl.new_field_infos_builder(soft_deletes_field, parent_field)?;

        for field in field_names {
            // Generate a random field type for this field.
            let field_type = self.random_field_type(random, &field)?;

            let mut store_term_vectors = false;
            let mut store_payloads = false;
            let mut omit_norms = false;

            if field_type.index_options() != &IndexOptions::None {
                store_term_vectors = field_type.store_term_vectors();
                omit_norms = field_type.omit_norms();
                if field_type.index_options() >= &IndexOptions::DocsAndFreqsAndPositions {
                    store_payloads = random.random_bool(0.5);
                }
            }

            let valid_doc_values = [
                DocValuesType::Numeric,
                DocValuesType::Sorted,
                DocValuesType::SortedNumeric,
                DocValuesType::SortedSet,
            ];
            let doc_values_skip_index_type =
                if valid_doc_values.contains(field_type.doc_values_type()) {
                    field_type.doc_values_skip_index_type()
                } else {
                    &DocValuesSkipIndexType::None
                };

            // Create a new FieldInfo for this field.
            let soft_deletes_field = match soft_deletes_field_clone {
                Some(ref s) => field == *s,
                None => false,
            };
            let parent_field = match parent_field_clone {
                Some(ref s) => field == *s,
                None => false,
            };
            let fi = FieldInfo::new(
                field.clone(),
                -1,
                store_term_vectors,
                omit_norms,
                store_payloads,
                *field_type.index_options(),
                *field_type.doc_values_type(),
                *doc_values_skip_index_type,
                -1,
                HashMap::new(),
                field_type.point_dimension_count(),
                field_type.point_index_dimension_count(),
                field_type.point_num_bytes(),
                field_type.vector_dimension(),
                *field_type.vector_encoding(),
                *field_type.vector_similarity_function(),
                soft_deletes_field,
                parent_field,
            );
            Self::add_attributes(&fi);
            builder.add(Arc::new(fi))?;
        }

        let infos = builder.finish()?;

        // Write the FieldInfos to the directory.
        codec.field_infos_format().write(
            dir.as_ref(),
            &segment_info,
            "",
            &infos,
            &IOContext::default_io_context()?,
        )?;

        // Read the FieldInfos back from the directory.
        let infos2 = codec.field_infos_format().read(
            dir.as_ref(),
            &segment_info,
            "",
            &IOContext::default_io_context()?,
        )?;

        // Verify that the written and read FieldInfos are equal.
        Self::assert_field_infos_equals(&infos, &infos2)?;
        Ok(())
    }

    fn get_vectors_max_dimensions(_field_name: &str) -> i32 {
        // TODO
        1024
    }

    fn random_field_type<R: Rng + ?Sized>(
        &self,
        random: &mut R,
        field_name: &str,
    ) -> Result<FieldType> {
        let mut field_type = FieldType::new();

        if random.random_bool(0.5) {
            field_type.set_index_options(
                IndexOptions::from_repr(random.random_range(0..IndexOptions::COUNT) as u8).unwrap(),
            )?;
            field_type.set_omit_norms(random.random_bool(0.5))?;

            if random.random_bool(0.5) {
                field_type.set_store_term_vectors(true)?;
                if field_type.index_options() >= &IndexOptions::DocsAndFreqsAndPositions {
                    field_type.set_store_term_vector_positions(random.random_bool(0.5))?;
                    field_type.set_store_term_vector_offsets(random.random_bool(0.5))?;
                    if field_type.store_term_vector_positions() {
                        field_type.set_store_term_vector_payloads(random.random_bool(0.5))?;
                    }
                }
            }
        }

        if random.random_bool(0.5) {
            let current =
                DocValuesType::from_repr(random.random_range(0..DocValuesType::COUNT) as u8)
                    .unwrap();
            field_type.set_doc_values_type(
                DocValuesType::from_repr(random.random_range(0..DocValuesType::COUNT) as u8)
                    .unwrap(),
            )?;
            if current == DocValuesType::Numeric
                || current == DocValuesType::SortedNumeric
                || current == DocValuesType::Sorted
                || current == DocValuesType::SortedSet
            {
                field_type.set_doc_values_skip_index_type(
                    if self.support_doc_values_skip_index() {
                        DocValuesSkipIndexType::Range
                    } else {
                        DocValuesSkipIndexType::None
                    },
                )?;
            }
        }

        if random.random_bool(0.5) {
            let dimension = 1 + random.random_range(0..MAX_DIMENSIONS);
            let index_dimension =
                1 + random.random_range(0..std::cmp::min(dimension, MAX_INDEX_DIMENSIONS));
            let dimension_num_bytes = 1 + random.random_range(0..MAX_NUM_BYTES);
            field_type.set_dimensions_all(dimension, index_dimension, dimension_num_bytes)?;
        }

        if random.random_bool(0.5) && Self::get_vectors_max_dimensions(field_name) > 0 {
            let max_dims = Self::get_vectors_max_dimensions(field_name);
            let dimension = 1 + random.random_range(0..max_dims);
            let similarity_function = VectorSimilarityFunction::from_repr(
                random.random_range(0..VectorSimilarityFunction::COUNT) as u8,
            )
            .unwrap();
            let encoding =
                VectorEncoding::from_repr(random.random_range(0..VectorEncoding::COUNT) as u8)
                    .unwrap();
            field_type.set_vector_attributes(dimension, encoding, similarity_function)?;
        }

        Ok(field_type)
    }
    /// Hook to add any codec attributes to fieldinfo instances added in this
    /// test.
    fn add_attributes(_fi: &FieldInfo) {}
    /// Asserts equality for the entirety of FieldInfos
    fn assert_field_infos_equals(expected: &FieldInfos, actual: &FieldInfos) -> Result<()> {
        assert_eq!(expected.size(), actual.size());

        for expected_field in expected.iter() {
            let actual_field = actual.field_info_by_number(expected_field.number)?;
            match actual_field {
                None => unreachable!("should be Some"),
                Some(actual_field) => {
                    Self::assert_field_info_equals(expected_field, &actual_field);
                },
            }
        }
        Ok(())
    }

    /// Asserts equality for two individual FieldInfo objects
    fn assert_field_info_equals(expected: &FieldInfo, actual: &FieldInfo) {
        assert_eq!(expected.number, actual.number);
        assert_eq!(expected.name, actual.name);
        assert_eq!(expected.get_doc_values_type(), actual.get_doc_values_type());
        assert_eq!(
            expected.doc_values_skip_index_type(),
            actual.doc_values_skip_index_type()
        );
        assert_eq!(expected.get_index_options(), actual.get_index_options());

        assert_eq!(expected.has_norms(), actual.has_norms());

        assert_eq!(expected.has_payloads(), actual.has_payloads());

        assert_eq!(expected.has_term_vectors(), actual.has_term_vectors());
        assert_eq!(expected.omits_norms(), actual.omits_norms());
        assert_eq!(expected.get_doc_values_gen(), actual.get_doc_values_gen());
    }

    fn new_segment_info<D: Directory, R: Rng + ?Sized>(
        random: &mut R,
        dir: Arc<D>,
        name: &str,
    ) -> Result<SegmentInfo<D>> {
        let min_version = if random.random_bool(0.5) {
            None
        } else {
            Some((*LATEST).clone())
        };
        let id = StringHelper::random_id();
        let value = SegmentInfo::new(
            dir,
            Some((*LATEST).clone()),
            min_version,
            name,
            10_000,
            false,
            false,
            HashMap::new(),
            id,
            HashMap::new(),
            None,
        )?;
        Ok(value)
    }
    // TODO: addRandomFields()

    fn create_field_info() -> FieldInfo {
        FieldInfo::new(
            "field",
            -1,
            false,
            false,
            false,
            IndexOptions::DocsAndFreqsAndPositions,
            DocValuesType::None,
            DocValuesSkipIndexType::None,
            -1,
            HashMap::new(),
            0,
            0,
            0,
            0,
            VectorEncoding::FLOAT32(4),
            VectorSimilarityFunction::Euclidean,
            false,
            false,
        )
    }
}
