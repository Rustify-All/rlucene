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
use crate::test_framework::core::util::lucene_test_case::{
  at_least, call_stack_contains_any_of, new_directory_shared, new_mock_directory,
};
use std::collections::{HashMap, HashSet};
use std::io::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rand::Rng;
use rand::RngExt;
use strum::EnumCount;

use crate::core::codecs::field_infos_format::FieldInfosFormat;
use crate::core::codecs::knn_vectors_format::KnnVectorsFormat;
use crate::core::codecs::{Codec, codec};
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
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::{LATEST, StringHelper, TryIntoInt};
use crate::test_framework::core::index::base_index_file_format_test_case::BaseIndexFileFormatTestCase;
use crate::test_framework::core::store::mock_directory_wrapper::{
  Failure, FakeIOException, MockDirectoryWrapper,
};
use crate::test_framework::core::util::index_package_access::{
  FieldInfosBuilder, IndexPackageAccess, IndexPackageAccessImpl,
};
use crate::test_framework::core::util::test_util::TestUtil;

pub trait BaseFieldInfoFormatTestCase: BaseIndexFileFormatTestCase {
  fn support_doc_values_skip_index(&self) -> bool {
    true
  }
  fn test_one_field<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let codec = self.get_codec()?;
    let segment_info = Self::new_segment_info(random, dir.clone(), "_123")?;

    let fi = Arc::new(Self::create_field_info()?);
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
    match infos2.field_info_by_name("field")? {
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
  fn test_immutable_attributes<R>(&self, _random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    test_not_required_in_rust_lucene!();
  }

  /// Test field infos write that hits exception immediately on open. make sure we get our exception
  /// back, no file handle leaks, etc.
  fn test_exception_on_create_output<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let enabled = Arc::new(AtomicBool::new(false));
    let dir = Arc::new(new_mock_directory(random)?);
    dir.fail_on(Box::new(FailOnCreateOutput {
      enabled: enabled.clone(),
    }));
    let codec = self.get_codec()?;
    let segment_info = Self::new_segment_info(random, dir.clone(), "_123")?;
    let fi = Arc::new(Self::create_field_info()?);
    Self::add_attributes(&fi);

    let infos = IndexPackageAccessImpl
      .new_field_infos_builder(None, None)?
      .add(fi)?
      .finish()?;

    enabled.store(true, Ordering::SeqCst);
    match codec.field_infos_format().write(
      dir.as_ref(),
      &segment_info,
      "",
      &infos,
      &IOContext::default_io_context()?,
    ) {
      Err(LuceneError::Io { source, .. }) | Err(LuceneError::IoWithPath { source, .. }) => {
        assert!(
          source
            .get_ref()
            .is_some_and(|source| source.is::<FakeIOException>()),
          "expected FakeIOException, got {source}"
        );
      },
      Err(error) => return Err(error),
      Ok(()) => panic!("expected FakeIOException"),
    }
    enabled.store(false, Ordering::SeqCst);

    dir.as_ref().close()?;
    Ok(())
  }
  /// Test field infos write that hits exception on close. make sure we get our exception back, no
  /// file handle leaks, etc.
  fn test_exception_on_close_output<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let enabled = Arc::new(AtomicBool::new(false));
    let dir = Arc::new(new_mock_directory(random)?);
    dir.fail_on(Box::new(FailOnCloseOutput {
      enabled: enabled.clone(),
    }));
    let codec = self.get_codec()?;
    let segment_info = Self::new_segment_info(random, dir.clone(), "_123")?;
    let fi = Arc::new(Self::create_field_info()?);
    Self::add_attributes(&fi);

    let infos = IndexPackageAccessImpl
      .new_field_infos_builder(None, None)?
      .add(fi)?
      .finish()?;

    enabled.store(true, Ordering::SeqCst);
    match codec.field_infos_format().write(
      dir.as_ref(),
      &segment_info,
      "",
      &infos,
      &IOContext::default_io_context()?,
    ) {
      Err(LuceneError::Io { source, .. }) | Err(LuceneError::IoWithPath { source, .. }) => {
        assert!(
          source
            .get_ref()
            .is_some_and(|source| source.is::<FakeIOException>()),
          "expected FakeIOException, got {source}"
        );
      },
      Err(error) => return Err(error),
      Ok(()) => panic!("expected FakeIOException"),
    }
    enabled.store(false, Ordering::SeqCst);

    dir.as_ref().close()?;
    Ok(())
  }
  /// Test field infos read that hits exception immediately on open. make sure we get our exception
  /// back, no file handle leaks, etc.
  fn test_exception_on_open_input<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let enabled = Arc::new(AtomicBool::new(false));
    let dir = Arc::new(new_mock_directory(random)?);
    dir.fail_on(Box::new(FailOnOpenInput {
      enabled: enabled.clone(),
    }));
    let codec = self.get_codec()?;
    let segment_info = Self::new_segment_info(random, dir.clone(), "_123")?;
    let fi = Arc::new(Self::create_field_info()?);
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

    enabled.store(true, Ordering::SeqCst);
    match codec.field_infos_format().read(
      dir.as_ref(),
      &segment_info,
      "",
      &IOContext::default_io_context()?,
    ) {
      Err(LuceneError::Io { source, .. }) | Err(LuceneError::IoWithPath { source, .. }) => {
        assert!(
          source
            .get_ref()
            .is_some_and(|source| source.is::<FakeIOException>()),
          "expected FakeIOException, got {source}"
        );
      },
      Err(error) => return Err(error),
      Ok(_) => panic!("expected FakeIOException"),
    }
    enabled.store(false, Ordering::SeqCst);

    dir.as_ref().close()?;
    Ok(())
  }
  /// Test field infos read that hits exception on close. make sure we get our exception back, no
  /// file handle leaks, etc.
  fn test_exception_on_close_input<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let enabled = Arc::new(AtomicBool::new(false));
    let dir = Arc::new(new_mock_directory(random)?);
    dir.fail_on(Box::new(FailOnCloseInput {
      enabled: enabled.clone(),
    }));
    let codec = self.get_codec()?;
    let segment_info = Self::new_segment_info(random, dir.clone(), "_123")?;
    let fi = Arc::new(Self::create_field_info()?);
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

    enabled.store(true, Ordering::SeqCst);
    match codec.field_infos_format().read(
      dir.as_ref(),
      &segment_info,
      "",
      &IOContext::default_io_context()?,
    ) {
      Err(LuceneError::Io { source, .. }) | Err(LuceneError::IoWithPath { source, .. }) => {
        assert!(
          source
            .get_ref()
            .is_some_and(|source| source.is::<FakeIOException>()),
          "expected FakeIOException, got {source}"
        );
      },
      Err(error) => return Err(error),
      Ok(_) => panic!("expected FakeIOException"),
    }
    enabled.store(false, Ordering::SeqCst);

    dir.as_ref().close()?;
    Ok(())
  }
  // Test field infos read/write with random fields, with different values.
  fn test_random<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let codec = self.get_codec()?;
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
      let doc_values_skip_index_type = if valid_doc_values.contains(field_type.doc_values_type()) {
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
      )?;
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

  fn get_vectors_max_dimensions(&self, field_name: &str) -> Result<usize> {
    self
      .get_codec()?
      .knn_vectors_format()
      .unwrap()
      .get_max_dimensions(field_name)
  }

  fn random_field_type<R>(&self, random: &mut R, field_name: &str) -> Result<FieldType>
  where
    R: Rng + ?Sized,
  {
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
        DocValuesType::from_repr(random.random_range(0..DocValuesType::COUNT) as u8).unwrap();
      field_type.set_doc_values_type(
        DocValuesType::from_repr(random.random_range(0..DocValuesType::COUNT) as u8).unwrap(),
      )?;
      if current == DocValuesType::Numeric
        || current == DocValuesType::SortedNumeric
        || current == DocValuesType::Sorted
        || current == DocValuesType::SortedSet
      {
        field_type.set_doc_values_skip_index_type(if self.support_doc_values_skip_index() {
          DocValuesSkipIndexType::Range
        } else {
          DocValuesSkipIndexType::None
        })?;
      }
    }

    if random.random_bool(0.5) {
      let dimension = 1 + random.random_range(0..MAX_DIMENSIONS);
      let index_dimension =
        1 + random.random_range(0..std::cmp::min(dimension, MAX_INDEX_DIMENSIONS));
      let dimension_num_bytes = 1 + random.random_range(0..MAX_NUM_BYTES);
      field_type.set_dimensions_with_index(dimension, index_dimension, dimension_num_bytes)?;
    }

    if random.random_bool(0.5) && self.get_vectors_max_dimensions(field_name)? > 0 {
      let max_dims = self.get_vectors_max_dimensions(field_name)?;
      let dimension = 1 + random.random_range(0..max_dims);
      let similarity_function = VectorSimilarityFunction::from_repr(
        random.random_range(0..VectorSimilarityFunction::COUNT) as u8,
      )
      .unwrap();
      let encoding =
        VectorEncoding::from_repr(random.random_range(0..VectorEncoding::COUNT) as u8).unwrap();
      field_type.set_vector_attributes(dimension.try_convert()?, encoding, similarity_function)?;
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

  fn new_segment_info<D, R>(random: &mut R, dir: Arc<D>, name: &str) -> Result<SegmentInfo<D>>
  where
    D: Directory,
    R: Rng + ?Sized,
  {
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
      Some(codec::get_default()),
      HashMap::new(),
      id,
      HashMap::new(),
      None,
    )?;
    Ok(value)
  }
  // TODO: addRandomFields()

  fn create_field_info() -> Result<FieldInfo> {
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
struct FailOnCreateOutput {
  enabled: Arc<AtomicBool>,
}

impl<D> Failure<D> for FailOnCreateOutput
where
  D: Directory,
{
  fn eval(&mut self, _dir: &MockDirectoryWrapper<D>) -> Result<()> {
    if self.enabled.load(Ordering::SeqCst) && call_stack_contains_any_of(&["create_output"]) {
      return Err(LuceneError::io(Error::other(FakeIOException)));
    }
    Ok(())
  }

  fn set_do_fail(&mut self) {
    self.enabled.store(true, Ordering::SeqCst);
  }

  fn clear_do_fail(&mut self) {
    self.enabled.store(false, Ordering::SeqCst);
  }
}

struct FailOnCloseOutput {
  enabled: Arc<AtomicBool>,
}

impl<D> Failure<D> for FailOnCloseOutput
where
  D: Directory,
{
  fn eval(&mut self, _dir: &MockDirectoryWrapper<D>) -> Result<()> {
    if self.enabled.load(Ordering::SeqCst) && call_stack_contains_any_of(&["close"]) {
      return Err(LuceneError::io(Error::other(FakeIOException)));
    }
    Ok(())
  }

  fn set_do_fail(&mut self) {
    self.enabled.store(true, Ordering::SeqCst);
  }

  fn clear_do_fail(&mut self) {
    self.enabled.store(false, Ordering::SeqCst);
  }
}

struct FailOnOpenInput {
  enabled: Arc<AtomicBool>,
}

impl<D> Failure<D> for FailOnOpenInput
where
  D: Directory,
{
  fn eval(&mut self, _dir: &MockDirectoryWrapper<D>) -> Result<()> {
    if self.enabled.load(Ordering::SeqCst) && call_stack_contains_any_of(&["open_input"]) {
      return Err(LuceneError::io(Error::other(FakeIOException)));
    }
    Ok(())
  }

  fn set_do_fail(&mut self) {
    self.enabled.store(true, Ordering::SeqCst);
  }

  fn clear_do_fail(&mut self) {
    self.enabled.store(false, Ordering::SeqCst);
  }
}

struct FailOnCloseInput {
  enabled: Arc<AtomicBool>,
}

impl<D> Failure<D> for FailOnCloseInput
where
  D: Directory,
{
  fn eval(&mut self, _dir: &MockDirectoryWrapper<D>) -> Result<()> {
    if self.enabled.load(Ordering::SeqCst) && call_stack_contains_any_of(&["close"]) {
      return Err(LuceneError::io(Error::other(FakeIOException)));
    }
    Ok(())
  }

  fn set_do_fail(&mut self) {
    self.enabled.store(true, Ordering::SeqCst);
  }

  fn clear_do_fail(&mut self) {
    self.enabled.store(false, Ordering::SeqCst);
  }
}
