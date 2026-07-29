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
use crate::core::codecs::knn_field_vectors_writer::VectorValueEnum;
use crate::core::codecs::knn_vectors_format::KnnVectorsFormat;
use crate::core::codecs::knn_vectors_writer::KnnVectorsWriter;
use crate::core::document::document::Document;
use crate::core::document::field::FieldBase;
use crate::core::document::field::Store;
use crate::core::document::knn_byte_vector_field::KnnByteVectorField;
use crate::core::document::knn_float_vector_field::KnnFloatVectorField;
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::document::string_field::StringField;
use crate::core::index::BytesRef;
use crate::core::index::byte_vector_values::ByteVectorValues;
use crate::core::index::check_index::Level;
use crate::core::index::directory_reader;
use crate::core::index::doc_values_skip_index_type::DocValuesSkipIndexType;
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::float_vector_values::FloatVectorValues;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::indexable_field::IndexableField;
use crate::core::index::indexable_field_type::IndexableFieldType;
use crate::core::index::knn_vector_values::{DocIndexIterator, KnnVectorValues};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::merge_policy::{MergePolicyEnum, OneMerge};
use crate::core::index::merge_scheduler::{MergeScheduler, MergeSchedulerEnum, MergeSource};
use crate::core::index::merge_trigger::MergeTrigger;
use crate::core::index::no_merge_policy::NoMergePolicy;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::term::Term;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::index::vector_encoding::VectorEncoding;
use crate::core::index::vector_similarity_function::VectorSimilarityFunction;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::knn_collector::KnnCollector;
use crate::core::search::sort::Sort;
use crate::core::search::sort_field::{SortField, SortFieldType};
use crate::core::search::top_knn_collector::TopKnnCollector;
use crate::core::search::total_hits::Relation::{EqualTo, GreaterThanOrEqualTo};
use crate::core::search::vector_scorer::VectorScorer;
use crate::core::store::FSDirectories;
use crate::core::store::directory::Directory;
use crate::core::util::LATEST;
use crate::core::util::StringHelper;
use crate::core::util::ToInt;
use crate::core::util::accountable::Accountable;
use crate::core::util::bits::Bits;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::info_stream::get_default_info_stream;
use crate::core::util::io_utils::IOUtils;
use crate::core::util::vector_util::VectorUtil;
use crate::test_framework::core::index::base_index_file_format_test_case::BaseIndexFileFormatTestCase;
use crate::test_framework::core::index::force_merge_policy::ForceMergePolicy;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, create_temp_dir, get_only_leaf_reader, new_directory_shared, new_index_writer_config,
  new_io_context, new_log_merge_policy,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::{Rng, RngExt};
use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use strum::EnumCount;

pub trait BaseKnnVectorsFormatTestCase: BaseIndexFileFormatTestCase {
  fn get_vectors_max_dimensions(&self, field_name: &str) -> Result<usize> {
    self
      .get_codec()?
      .knn_vectors_format()
      .unwrap()
      .get_max_dimensions(field_name)
  }

  fn test_field_constructor<R>(&self, _random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let v = vec![0.0_f32; 1];
    let field = KnnFloatVectorField::new("f", v.clone())?;
    assert_eq!(1, field.field_type().vector_dimension());
    assert_eq!(
      &VectorSimilarityFunction::Euclidean,
      field.field_type().vector_similarity_function()
    );
    match field.vector_value()? {
      VectorValueEnum::Float(actual) => assert_eq!(v.as_slice(), actual.as_slice()),
      _ => unreachable!(""),
    }
    Ok(())
  }

  fn test_field_constructor_exceptions<R>(&self, _random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let res = KnnFloatVectorField::new("f", vec![]);
    assert!(matches!(res, Err(LuceneError::IllegalArgument(_))));
    Ok(())
  }

  fn test_field_set_value<R>(&self, _random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let mut field = KnnFloatVectorField::new("f", vec![0.0])?;
    let v1 = vec![1.0_f32];
    field.set_vector_value(v1.clone())?;
    match field.vector_value()? {
      VectorValueEnum::Float(actual) => assert_eq!(v1.as_slice(), actual.as_slice()),
      _ => unreachable!(""),
    }

    let err = field.set_vector_value(vec![1.0, 2.0]).unwrap_err();
    assert_eq!(
      "value length 2 must match field dimension 1",
      err.to_string()
    );
    Ok(())
  }

  fn test_illegal_dim_change_two_docs<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    {
      let dir = new_directory_shared(random)?;
      let w = IndexWriter::new(dir, new_index_writer_config(random)?)?;

      let mut doc = Document::new();
      doc.add(KnnFloatVectorField::with_similarity_function(
        "f",
        vec![0.0; 4],
        VectorSimilarityFunction::DotProduct,
      )?);
      w.add_document(doc)?;

      let mut doc2 = Document::new();
      doc2.add(KnnFloatVectorField::with_similarity_function(
        "f",
        vec![0.0; 6],
        VectorSimilarityFunction::DotProduct,
      )?);
      let err = w.add_document(doc2).unwrap_err();
      assert_eq!(
        "Inconsistency of field data structures across documents for field [f] of doc [1]. vector dimension: expected '4', but it has '6'.",
        err.to_string()
      );
    }

    {
      let dir = new_directory_shared(random)?;
      let w = IndexWriter::new(dir, new_index_writer_config(random)?)?;

      let mut doc = Document::new();
      doc.add(KnnFloatVectorField::with_similarity_function(
        "f",
        vec![0.0; 4],
        VectorSimilarityFunction::DotProduct,
      )?);
      w.add_document(doc)?;
      w.commit()?;

      let mut doc2 = Document::new();
      doc2.add(KnnFloatVectorField::with_similarity_function(
        "f",
        vec![0.0; 6],
        VectorSimilarityFunction::DotProduct,
      )?);
      let err = w.add_document(doc2).unwrap_err();
      assert_eq!(
        format!(
          "cannot change field \"f\" from vector dimension=4, vector encoding={:?}, vector similarity function={:?} to inconsistent vector dimension=6, vector encoding={:?}, vector similarity function={:?}",
          VectorEncoding::FLOAT32(4),
          VectorSimilarityFunction::DotProduct,
          VectorEncoding::FLOAT32(4),
          VectorSimilarityFunction::DotProduct
        ),
        err.to_string()
      );
    }

    Ok(())
  }

  fn test_illegal_similarity_function_change<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    {
      let dir = new_directory_shared(random)?;
      let w = IndexWriter::new(dir, new_index_writer_config(random)?)?;

      let mut doc = Document::new();
      doc.add(KnnFloatVectorField::with_similarity_function(
        "f",
        vec![0.0; 4],
        VectorSimilarityFunction::DotProduct,
      )?);
      w.add_document(doc)?;

      let mut doc2 = Document::new();
      doc2.add(KnnFloatVectorField::with_similarity_function(
        "f",
        vec![0.0; 4],
        VectorSimilarityFunction::Euclidean,
      )?);
      let err = w.add_document(doc2).unwrap_err();
      assert_eq!(
        format!(
          "Inconsistency of field data structures across documents for field [f] of doc [1]. vector similarity function: expected '{}', but it has '{}'.",
          VectorSimilarityFunction::DotProduct,
          VectorSimilarityFunction::Euclidean
        ),
        err.to_string()
      );
    }

    {
      let dir = new_directory_shared(random)?;
      let w = IndexWriter::new(dir, new_index_writer_config(random)?)?;

      let mut doc = Document::new();
      doc.add(KnnFloatVectorField::with_similarity_function(
        "f",
        vec![0.0; 4],
        VectorSimilarityFunction::DotProduct,
      )?);
      w.add_document(doc)?;
      w.commit()?;

      let mut doc2 = Document::new();
      doc2.add(KnnFloatVectorField::with_similarity_function(
        "f",
        vec![0.0; 4],
        VectorSimilarityFunction::Euclidean,
      )?);
      let err = w.add_document(doc2).unwrap_err();
      assert_eq!(
        format!(
          "cannot change field \"f\" from vector dimension=4, vector encoding={:?}, vector similarity function={:?} to inconsistent vector dimension=4, vector encoding={:?}, vector similarity function={:?}",
          VectorEncoding::FLOAT32(4),
          VectorSimilarityFunction::DotProduct,
          VectorEncoding::FLOAT32(4),
          VectorSimilarityFunction::Euclidean
        ),
        err.to_string()
      );
    }

    Ok(())
  }

  fn test_illegal_dim_change_two_writers<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;

    {
      let w = IndexWriter::new(dir.clone(), new_index_writer_config(random)?)?;
      let mut doc = Document::new();
      doc.add(KnnFloatVectorField::with_similarity_function(
        "f",
        vec![0.0; 4],
        VectorSimilarityFunction::DotProduct,
      )?);
      w.add_document(doc)?;
      w.close()?;
    }

    {
      let w = IndexWriter::new(dir, new_index_writer_config(random)?)?;
      let mut doc = Document::new();
      doc.add(KnnFloatVectorField::with_similarity_function(
        "f",
        vec![0.0; 2],
        VectorSimilarityFunction::DotProduct,
      )?);
      let err = w.add_document(doc).unwrap_err();
      assert_eq!(
        format!(
          "cannot change field \"f\" from vector dimension=4, vector encoding={:?}, vector similarity function={:?} to inconsistent vector dimension=2, vector encoding={:?}, vector similarity function={:?}",
          VectorEncoding::FLOAT32(4),
          VectorSimilarityFunction::DotProduct,
          VectorEncoding::FLOAT32(4),
          VectorSimilarityFunction::DotProduct
        ),
        err.to_string()
      );
    }

    Ok(())
  }

  fn test_merging_with_different_knn_fields<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let ex = Arc::new(AtomicBool::new(false));
    let merge_scheduler = TestMergeScheduler::new(ex.clone());
    let mut iwc = new_index_writer_config(random)?;
    iwc.set_merge_scheduler(MergeSchedulerEnum::KnnMergeScheduler(merge_scheduler));
    let mp = iwc.get_merge_policy().clone();
    iwc.set_merge_policy(MergePolicyEnum::Force(ForceMergePolicy::new(mp)));

    let writer = IndexWriter::new(dir, iwc)?;
    for i in 0..10 {
      let mut doc = Document::new();
      doc.add(KnnFloatVectorField::new(
        "field",
        vec![i as f32, (i + 1) as f32, (i + 2) as f32, (i + 3) as f32],
      )?);
      writer.add_document(doc)?;
    }
    writer.commit()?;

    for i in 0..10 {
      let mut doc = Document::new();
      doc.add(KnnFloatVectorField::new(
        "otherVector",
        vec![i as f32, i as f32, i as f32, i as f32],
      )?);
      writer.add_document(doc)?;
    }
    writer.commit()?;
    writer.force_merge(1)?;
    writer.close()?;

    assert!(!ex.load(Ordering::SeqCst));
    Ok(())
  }

  fn test_merging_with_different_byte_knn_fields<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let ex = Arc::new(AtomicBool::new(false));
    let merge_scheduler = TestMergeScheduler::new(ex.clone());
    let mut iwc = new_index_writer_config(random)?;
    iwc.set_merge_scheduler(MergeSchedulerEnum::KnnMergeScheduler(merge_scheduler));
    let mp = iwc.get_merge_policy().clone();
    iwc.set_merge_policy(MergePolicyEnum::Force(ForceMergePolicy::new(mp)));

    let writer = IndexWriter::new(dir, iwc)?;
    for i in 0..10 {
      let mut doc = Document::new();
      doc.add(KnnByteVectorField::new(
        "field",
        vec![i as u8, i as u8, i as u8, i as u8],
      )?);
      writer.add_document(doc)?;
    }
    writer.commit()?;

    for i in 0..10 {
      let mut doc = Document::new();
      doc.add(KnnByteVectorField::new(
        "otherVector",
        vec![i as u8, i as u8, i as u8, i as u8],
      )?);
      writer.add_document(doc)?;
    }
    writer.commit()?;
    writer.force_merge(1)?;
    writer.close()?;

    assert!(!ex.load(Ordering::SeqCst));
    Ok(())
  }

  fn test_writer_ram_estimate<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let field_infos = Arc::new(FieldInfos::new(vec![])?);
    let dir = new_directory_shared(random)?;
    let codec = self.get_codec()?;
    let segment_info = SegmentInfo::new(
      dir.clone(),
      Some((*LATEST).clone()),
      Some((*LATEST).clone()),
      "0",
      10000,
      false,
      false,
      Some(codec.clone()),
      HashMap::new(),
      StringHelper::random_id(),
      HashMap::new(),
      None,
    )?;
    let io_context = new_io_context(random)?;
    let state = SegmentWriteState::new(
      get_default_info_stream(),
      dir.as_ref(),
      field_infos,
      &io_context,
    );
    let format = codec.knn_vectors_format()?;
    let mut writer = format.fields_writer(&state, &segment_info)?;
    let ram_bytes_used = writer.ram_bytes_used()?;
    let mut dim = random.random_range(1..=64);
    if dim % 2 == 1 {
      dim += 1;
    }
    let num_docs = at_least(random, 100);
    let field_writer_idx = writer.add_field(
      &state,
      &segment_info,
      Arc::new(FieldInfo::new(
        "fieldA",
        0,
        false,
        false,
        false,
        IndexOptions::None,
        DocValuesType::None,
        DocValuesSkipIndexType::None,
        -1,
        HashMap::new(),
        0,
        0,
        0,
        dim,
        VectorEncoding::FLOAT32(4),
        VectorSimilarityFunction::DotProduct,
        false,
        false,
      )?),
    )?;
    for i in 0..num_docs {
      let vector = VectorValueEnum::Float(Self::random_vector(random, dim as usize));
      writer.add_value(i, &vector, field_writer_idx)?;
    }
    let ram_bytes_used2 = writer.ram_bytes_used()?;
    assert!(ram_bytes_used2 > ram_bytes_used);
    assert!(
      ram_bytes_used2
        > dim as i64 * num_docs as i64 * VectorEncoding::FLOAT32(4).byte_size() as i64
    );
    writer.finish()?;
    Ok(())
  }

  fn test_illegal_similarity_function_change_two_writers<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;

    {
      let w = IndexWriter::new(dir.clone(), new_index_writer_config(random)?)?;
      let mut doc = Document::new();
      doc.add(KnnFloatVectorField::with_similarity_function(
        "f",
        vec![0.0; 4],
        VectorSimilarityFunction::DotProduct,
      )?);
      w.add_document(doc)?;
      w.close()?;
    }

    {
      let w = IndexWriter::new(dir, new_index_writer_config(random)?)?;
      let mut doc2 = Document::new();
      doc2.add(KnnFloatVectorField::with_similarity_function(
        "f",
        vec![0.0; 4],
        VectorSimilarityFunction::Euclidean,
      )?);
      let err = w.add_document(doc2).unwrap_err();
      assert_eq!(
        format!(
          "cannot change field \"f\" from vector dimension=4, vector encoding={:?}, vector similarity function={:?} to inconsistent vector dimension=4, vector encoding={:?}, vector similarity function={:?}",
          VectorEncoding::FLOAT32(4),
          VectorSimilarityFunction::DotProduct,
          VectorEncoding::FLOAT32(4),
          VectorSimilarityFunction::Euclidean
        ),
        err.to_string()
      );
    }

    Ok(())
  }

  fn test_add_indexes_directory0<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let field_name = "field";
    let mut doc = Document::new();
    doc.add(KnnFloatVectorField::with_similarity_function(
      field_name,
      vec![0.0; 4],
      VectorSimilarityFunction::DotProduct,
    )?);

    let dir = new_directory_shared(random)?;
    let dir2 = new_directory_shared(random)?;

    {
      let iwc = new_index_writer_config(random)?;
      let w = IndexWriter::new(dir.clone(), iwc)?;
      w.add_document(doc.clone())?;
      w.close()?;
    }

    {
      let iwc = new_index_writer_config(random)?;
      let w2 = IndexWriter::new(dir2.clone(), iwc)?;
      w2.add_indexes_from_directory(std::slice::from_ref(&dir))?;
      w2.force_merge(1)?;

      let reader = directory_reader::open_from_writer(&w2)?;
      let r = get_only_leaf_reader(&reader)?;
      let vector_values = r.get_float_vector_values(field_name)?.unwrap();
      let mut iterator = vector_values.iterator()?;
      assert_eq!(0, iterator.next_doc()?);
      assert_eq!(0.0, vector_values.vector_value(0)?.as_floats()?[0]);
      assert_eq!(NO_MORE_DOCS, iterator.next_doc()?);
      reader.close()?;

      w2.close()?;
    }

    Ok(())
  }

  fn test_add_indexes_directory1<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let field_name = "field";
    let mut doc = Document::new();

    let dir = new_directory_shared(random)?;
    let dir2 = new_directory_shared(random)?;

    {
      let iwc = new_index_writer_config(random)?;
      let w = IndexWriter::new(dir.clone(), iwc)?;
      w.add_document(doc.clone())?;
      w.close()?;
    }

    doc.add(KnnFloatVectorField::with_similarity_function(
      field_name,
      vec![0.0; 4],
      VectorSimilarityFunction::DotProduct,
    )?);

    {
      let iwc = new_index_writer_config(random)?;
      let w2 = IndexWriter::new(dir2.clone(), iwc)?;
      w2.add_document(doc)?;
      w2.add_indexes_from_directory(std::slice::from_ref(&dir))?;
      w2.force_merge(1)?;

      let reader = directory_reader::open_from_writer(&w2)?;
      let r = get_only_leaf_reader(&reader)?;
      let vector_values = r.get_float_vector_values(field_name)?.unwrap();
      let mut iterator = vector_values.iterator()?;
      assert_ne!(NO_MORE_DOCS, iterator.next_doc()?);
      assert_eq!(
        0.0,
        vector_values
          .vector_value(iterator.index()? as usize)?
          .as_floats()?[0]
      );
      assert_eq!(NO_MORE_DOCS, iterator.next_doc()?);
      reader.close()?;

      w2.close()?;
    }

    Ok(())
  }

  fn test_add_indexes_directory01<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let field_name = "field";
    let mut vector = vec![0.0f32; 2];
    let mut doc = Document::new();
    doc.add(KnnFloatVectorField::with_similarity_function(
      field_name,
      vector.clone(),
      VectorSimilarityFunction::DotProduct,
    )?);

    let dir = new_directory_shared(random)?;
    let dir2 = new_directory_shared(random)?;

    {
      let iwc = new_index_writer_config(random)?;
      let w = IndexWriter::new(dir.clone(), iwc)?;
      w.add_document(doc.clone())?;
      w.close()?;
    }

    {
      let iwc = new_index_writer_config(random)?;
      let w2 = IndexWriter::new(dir2.clone(), iwc)?;
      vector[0] = 1.0;
      vector[1] = 1.0;

      let mut doc2 = Document::new();
      doc2.add(KnnFloatVectorField::with_similarity_function(
        field_name,
        vector,
        VectorSimilarityFunction::DotProduct,
      )?);

      w2.add_document(doc2)?;
      w2.add_indexes_from_directory(std::slice::from_ref(&dir))?;
      w2.force_merge(1)?;

      let reader = directory_reader::open_from_writer(&w2)?;
      let r = get_only_leaf_reader(&reader)?;
      let vector_values = r.get_float_vector_values(field_name)?.unwrap();
      let mut iterator = vector_values.iterator()?;

      assert_eq!(0, iterator.next_doc()?);
      let mut value = vector_values.vector_value(0)?.as_floats()?[0];
      assert!(value == 0.0 || value == 1.0);

      assert_eq!(1, iterator.next_doc()?);
      value += vector_values.vector_value(1)?.as_floats()?[0];
      assert_eq!(1.0, value);

      reader.close()?;
      w2.close()?;
    }

    Ok(())
  }

  fn test_illegal_dim_change_via_add_indexes_directory<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let dir2 = new_directory_shared(random)?;

    {
      let iwc = new_index_writer_config(random)?;
      let w = IndexWriter::new(dir.clone(), iwc)?;
      let mut doc = Document::new();
      doc.add(KnnFloatVectorField::with_similarity_function(
        "f",
        vec![0.0; 4],
        VectorSimilarityFunction::DotProduct,
      )?);
      w.add_document(doc)?;
      w.close()?;
    }

    {
      let iwc = new_index_writer_config(random)?;
      let w2 = IndexWriter::new(dir2.clone(), iwc)?;
      let mut doc = Document::new();
      doc.add(KnnFloatVectorField::with_similarity_function(
        "f",
        vec![0.0; 6],
        VectorSimilarityFunction::DotProduct,
      )?);
      w2.add_document(doc)?;

      let err = w2.add_indexes_from_directory(std::slice::from_ref(&dir));
      assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
      assert_eq!(
        "cannot change field \"f\" from vector dimension=6, vector encoding=FLOAT32(4), vector similarity function=DotProduct to inconsistent vector dimension=4, vector encoding=FLOAT32(4), vector similarity function=DotProduct",
        err.unwrap_err().to_string()
      );
    }

    Ok(())
  }

  fn test_illegal_similarity_function_change_via_add_indexes_directory<R>(
    &self,
    random: &mut R,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let dir2 = new_directory_shared(random)?;

    {
      let iwc = new_index_writer_config(random)?;
      let w = IndexWriter::new(dir.clone(), iwc)?;
      let mut doc = Document::new();
      doc.add(KnnFloatVectorField::with_similarity_function(
        "f",
        vec![0.0; 4],
        VectorSimilarityFunction::DotProduct,
      )?);
      w.add_document(doc)?;
      w.close()?;
    }

    {
      let iwc = new_index_writer_config(random)?;
      let w2 = IndexWriter::new(dir2.clone(), iwc)?;
      let mut doc = Document::new();
      doc.add(KnnFloatVectorField::with_similarity_function(
        "f",
        vec![0.0; 4],
        VectorSimilarityFunction::Euclidean,
      )?);
      w2.add_document(doc)?;

      let err = w2.add_indexes_from_directory(std::slice::from_ref(&dir));
      assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
      assert_eq!(
        "cannot change field \"f\" from vector dimension=4, vector encoding=FLOAT32(4), vector similarity function=Euclidean to inconsistent vector dimension=4, vector encoding=FLOAT32(4), vector similarity function=DotProduct",
        err.unwrap_err().to_string()
      );
    }

    Ok(())
  }

  fn test_illegal_dim_change_via_add_indexes_codec_reader<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let dir2 = new_directory_shared(random)?;

    {
      let iwc = new_index_writer_config(random)?;
      let w = IndexWriter::new(dir.clone(), iwc)?;
      let mut doc = Document::new();
      doc.add(KnnFloatVectorField::with_similarity_function(
        "f",
        vec![0.0; 4],
        VectorSimilarityFunction::DotProduct,
      )?);
      w.add_document(doc)?;
      w.close()?;
    }

    {
      let iwc = new_index_writer_config(random)?;
      let w2 = IndexWriter::new(dir2.clone(), iwc)?;
      let mut doc = Document::new();
      doc.add(KnnFloatVectorField::with_similarity_function(
        "f",
        vec![0.0; 6],
        VectorSimilarityFunction::DotProduct,
      )?);
      w2.add_document(doc)?;
      let reader = directory_reader::open(dir.clone())?;
      let leaf = get_only_leaf_reader(&reader)?;
      let err = w2.add_indexes_from_codec_readers(vec![leaf]);
      assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
      assert_eq!(
        "cannot change field \"f\" from vector dimension=6, vector encoding=FLOAT32(4), vector similarity function=DotProduct to inconsistent vector dimension=4, vector encoding=FLOAT32(4), vector similarity function=DotProduct",
        err.unwrap_err().to_string()
      );
      reader.close()?;
      w2.close()?;
    }

    Ok(())
  }

  fn test_illegal_similarity_function_change_via_add_indexes_codec_reader<R>(
    &self,
    random: &mut R,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let dir2 = new_directory_shared(random)?;

    {
      let iwc = new_index_writer_config(random)?;
      let w = IndexWriter::new(dir.clone(), iwc)?;
      let mut doc = Document::new();
      doc.add(KnnFloatVectorField::with_similarity_function(
        "f",
        vec![0.0; 4],
        VectorSimilarityFunction::DotProduct,
      )?);
      w.add_document(doc)?;
      w.close()?;
    }

    {
      let iwc = new_index_writer_config(random)?;
      let w2 = IndexWriter::new(dir2.clone(), iwc)?;
      let mut doc = Document::new();
      doc.add(KnnFloatVectorField::with_similarity_function(
        "f",
        vec![0.0; 4],
        VectorSimilarityFunction::Euclidean,
      )?);
      w2.add_document(doc)?;
      let reader = directory_reader::open(dir.clone())?;
      let leaf = get_only_leaf_reader(&reader)?;
      let err = w2.add_indexes_from_codec_readers(vec![leaf]);
      assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
      assert_eq!(
        "cannot change field \"f\" from vector dimension=4, vector encoding=FLOAT32(4), vector similarity function=Euclidean to inconsistent vector dimension=4, vector encoding=FLOAT32(4), vector similarity function=DotProduct",
        err.unwrap_err().to_string()
      );
      reader.close()?;
      w2.close()?;
    }

    Ok(())
  }

  fn test_illegal_dim_change_via_add_indexes_slow_codec_reader<R>(
    &self,
    random: &mut R,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let dir2 = new_directory_shared(random)?;

    {
      let iwc = new_index_writer_config(random)?;
      let w = IndexWriter::new(dir.clone(), iwc)?;
      let mut doc = Document::new();
      doc.add(KnnFloatVectorField::with_similarity_function(
        "f",
        vec![0.0; 4],
        VectorSimilarityFunction::DotProduct,
      )?);
      w.add_document(doc)?;
      w.close()?;
    }

    {
      let iwc = new_index_writer_config(random)?;
      let w2 = IndexWriter::new(dir2.clone(), iwc)?;
      let mut doc = Document::new();
      doc.add(KnnFloatVectorField::with_similarity_function(
        "f",
        vec![0.0; 6],
        VectorSimilarityFunction::DotProduct,
      )?);
      w2.add_document(doc)?;
      let reader = directory_reader::open(dir.clone())?;
      let err = TestUtil::add_indexes_slowly(&w2, &[&reader]);
      assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
      assert_eq!(
        "cannot change field \"f\" from vector dimension=6, vector encoding=FLOAT32(4), vector similarity function=DotProduct to inconsistent vector dimension=4, vector encoding=FLOAT32(4), vector similarity function=DotProduct",
        err.unwrap_err().to_string()
      );
      reader.close()?;
      w2.close()?;
    }

    Ok(())
  }

  fn test_illegal_similarity_function_change_via_add_indexes_slow_codec_reader<R>(
    &self,
    random: &mut R,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let dir2 = new_directory_shared(random)?;

    {
      let iwc = new_index_writer_config(random)?;
      let w = IndexWriter::new(dir.clone(), iwc)?;
      let mut doc = Document::new();
      doc.add(KnnFloatVectorField::with_similarity_function(
        "f",
        vec![0.0; 4],
        VectorSimilarityFunction::DotProduct,
      )?);
      w.add_document(doc)?;
      w.close()?;
    }

    {
      let iwc = new_index_writer_config(random)?;
      let w2 = IndexWriter::new(dir2.clone(), iwc)?;
      let mut doc = Document::new();
      doc.add(KnnFloatVectorField::with_similarity_function(
        "f",
        vec![0.0; 4],
        VectorSimilarityFunction::Euclidean,
      )?);
      w2.add_document(doc)?;
      let reader = directory_reader::open(dir.clone())?;
      let err = TestUtil::add_indexes_slowly(&w2, &[&reader]);
      assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
      assert_eq!(
        "cannot change field \"f\" from vector dimension=4, vector encoding=FLOAT32(4), vector similarity function=Euclidean to inconsistent vector dimension=4, vector encoding=FLOAT32(4), vector similarity function=DotProduct",
        err.unwrap_err().to_string()
      );
      reader.close()?;
      w2.close()?;
    }

    Ok(())
  }

  fn test_illegal_multiple_values<R>(&self, _random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(_random)?;
    let w = IndexWriter::new(dir, new_index_writer_config(_random)?)?;
    let mut doc = Document::new();
    doc.add(KnnFloatVectorField::with_similarity_function(
      "f",
      vec![0.0; 4],
      VectorSimilarityFunction::DotProduct,
    )?);
    doc.add(KnnFloatVectorField::with_similarity_function(
      "f",
      vec![0.0; 4],
      VectorSimilarityFunction::DotProduct,
    )?);
    let err = w.add_document(doc).unwrap_err();
    assert_eq!(
      "VectorValuesField \"f\" appears more than once in this document (only one value is allowed per field)",
      err.to_string()
    );
    Ok(())
  }

  fn test_illegal_dimension_too_large<R>(&self, _random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(_random)?;
    let w = IndexWriter::new(dir, new_index_writer_config(_random)?)?;
    let max_dim = self.get_vectors_max_dimensions("f")?;

    let mut doc = Document::new();
    doc.add(KnnFloatVectorField::with_similarity_function(
      "f",
      vec![0.0; max_dim + 1],
      VectorSimilarityFunction::DotProduct,
    )?);
    let exc = w.add_document(doc).unwrap_err();
    assert!(
      exc
        .to_string()
        .contains(&format!("vector's dimensions must be <= [{max_dim}]"))
    );

    let mut doc2 = Document::new();
    doc2.add(KnnFloatVectorField::with_similarity_function(
      "f",
      vec![0.0; 2],
      VectorSimilarityFunction::DotProduct,
    )?);
    w.add_document(doc2)?;

    let mut doc3 = Document::new();
    doc3.add(KnnFloatVectorField::with_similarity_function(
      "f",
      vec![0.0; max_dim + 1],
      VectorSimilarityFunction::DotProduct,
    )?);
    let exc = w.add_document(doc3).unwrap_err();
    let msg = exc.to_string();
    assert!(
      msg.contains("Inconsistency of field data structures across documents for field [f]")
        || msg.contains(&format!("vector's dimensions must be <= [{max_dim}]"))
    );
    w.flush()?;

    let mut doc4 = Document::new();
    doc4.add(KnnFloatVectorField::with_similarity_function(
      "f",
      vec![0.0; max_dim + 1],
      VectorSimilarityFunction::DotProduct,
    )?);
    let exc = w.add_document(doc4).unwrap_err();
    assert!(
      exc
        .to_string()
        .contains(&format!("vector's dimensions must be <= [{max_dim}]"))
    );
    Ok(())
  }

  fn test_illegal_empty_vector<R>(&self, _random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(_random)?;
    let w = IndexWriter::new(dir, new_index_writer_config(_random)?)?;

    let e = match KnnFloatVectorField::with_similarity_function(
      "f",
      vec![],
      VectorSimilarityFunction::Euclidean,
    ) {
      Ok(_) => unreachable!("expected empty vector creation to fail"),
      Err(err) => err,
    };
    assert_eq!("cannot index an empty vector", e.to_string());

    let mut doc2 = Document::new();
    doc2.add(KnnFloatVectorField::with_similarity_function(
      "f",
      vec![0.0; 2],
      VectorSimilarityFunction::Euclidean,
    )?);
    w.add_document(doc2)?;
    Ok(())
  }

  fn test_different_codecs1<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;

    {
      let w = IndexWriter::new(dir.clone(), new_index_writer_config(random)?)?;
      let mut doc = Document::new();
      doc.add(KnnFloatVectorField::with_similarity_function(
        "f",
        vec![0.0; 4],
        VectorSimilarityFunction::DotProduct,
      )?);
      w.add_document(doc)?;
      w.close()?;
    }

    {
      let iwc = new_index_writer_config(random)?;
      // TODO set_codec 未实现
      // iwc.set_codec(Codec::for_name("SimpleText")?);
      let w = IndexWriter::new(dir, iwc)?;
      let mut doc = Document::new();
      doc.add(KnnFloatVectorField::with_similarity_function(
        "f",
        vec![0.0; 4],
        VectorSimilarityFunction::DotProduct,
      )?);
      w.add_document(doc)?;
      w.force_merge(1)?;
      w.close()?;
    }

    Ok(())
  }

  fn test_different_codecs2<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let iwc = new_index_writer_config(random)?;
    // TODO set_codec 未实现
    // iwc.set_codec(Codec::for_name("SimpleText")?);

    let dir = new_directory_shared(random)?;

    {
      let w = IndexWriter::new(dir.clone(), iwc)?;
      let mut doc = Document::new();
      doc.add(KnnFloatVectorField::with_similarity_function(
        "f",
        vec![0.0; 4],
        VectorSimilarityFunction::DotProduct,
      )?);
      w.add_document(doc)?;
      w.close()?;
    }

    {
      let w = IndexWriter::new(dir, new_index_writer_config(random)?)?;
      let mut doc = Document::new();
      doc.add(KnnFloatVectorField::with_similarity_function(
        "f",
        vec![0.0; 4],
        VectorSimilarityFunction::DotProduct,
      )?);
      w.add_document(doc)?;
      w.force_merge(1)?;
      w.close()?;
    }

    Ok(())
  }

  fn test_invalid_knn_vector_field_usage<R>(&self, _random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let mut field = KnnFloatVectorField::with_similarity_function(
      "field",
      vec![0.0; 2],
      VectorSimilarityFunction::Euclidean,
    )?;

    assert!(field.set_int_value(14).is_err());

    let err = field.set_vector_value(vec![0.0; 1]).unwrap_err();
    assert!(matches!(err, LuceneError::IllegalArgument(_)));

    assert_eq!(None, field.numeric_value()?);
    Ok(())
  }

  fn test_delete_all_vector_docs<R>(&self, _random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(_random)?;
    let w = IndexWriter::new(dir, new_index_writer_config(_random)?)?;

    let mut doc = Document::new();
    doc.add(StringField::from_string("id", "0", Store::No)?);
    doc.add(KnnFloatVectorField::with_similarity_function(
      "v",
      vec![2.0, 3.0, 5.0, 6.0],
      VectorSimilarityFunction::DotProduct,
    )?);
    w.add_document(doc)?;
    w.add_document(Document::new())?;
    w.commit()?;

    {
      let reader = directory_reader::open_from_writer(&w)?;
      let leaf = get_only_leaf_reader(reader)?;
      let values = leaf.get_float_vector_values("v")?.expect("vector values");
      assert_eq!(1, values.size());
    }

    w.delete_documents_with_terms(vec![Term::from_text("id", "0")])?;
    w.force_merge(1)?;
    {
      let reader = directory_reader::open_from_writer(&w)?;
      let leaf = get_only_leaf_reader(reader)?;
      let values = leaf.get_float_vector_values("v")?.expect("vector values");
      assert_eq!(0, values.size());

      let mut collector = TopKnnCollector::new(1, i32::MAX as usize)?;
      leaf.search_nearest_vectors_f32(
        "v",
        vec![1.0, 0.0, 0.0, 0.0],
        &mut collector,
        leaf.get_live_docs()?,
      )?;
      let top_docs = collector.top_docs()?;
      assert_eq!(0, top_docs.score_docs.len());
      assert_eq!(NO_MORE_DOCS, values.iterator()?.next_doc()?);
    }
    Ok(())
  }

  fn test_knn_vector_field_missing_from_one_segment<R>(&self, _random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = Arc::new(FSDirectories::open(create_temp_dir()?.keep())?);
    let w = IndexWriter::new(dir.clone(), new_index_writer_config(_random)?)?;

    let mut doc = Document::new();
    doc.add(StringField::from_string("id", "0", Store::No)?);
    doc.add(KnnFloatVectorField::with_similarity_function(
      "v0",
      vec![2.0, 3.0, 5.0, 6.0],
      VectorSimilarityFunction::DotProduct,
    )?);
    w.add_document(doc)?;
    w.commit()?;

    let mut doc = Document::new();
    doc.add(KnnFloatVectorField::with_similarity_function(
      "v1",
      vec![2.0, 3.0, 5.0, 6.0],
      VectorSimilarityFunction::DotProduct,
    )?);
    w.add_document(doc)?;
    w.force_merge(1)?;
    w.close()?;
    dir.close()
  }
  fn test_sparse_vectors<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_docs = at_least(random, 1000);
    let num_fields = TestUtil::next_int(random, 1, 10);
    let mut field_doc_counts = vec![0i32; num_fields as usize];
    let mut field_totals = vec![0f64; num_fields as usize];
    let mut field_dims = vec![0usize; num_fields as usize];
    let mut field_similarity_functions =
      vec![VectorSimilarityFunction::Euclidean; num_fields as usize];
    let mut field_vector_encodings = vec![VectorEncoding::FLOAT32(0); num_fields as usize];

    for i in 0..num_fields as usize {
      let mut dim = random.random_range(1..=20) as usize;
      if !dim.is_multiple_of(2) {
        dim += 1;
      }
      field_dims[i] = dim;
      field_similarity_functions[i] = self.random_similarity(random);
      field_vector_encodings[i] = self.random_vector_encoding(random);
    }

    let dir = new_directory_shared(random)?;
    let config = new_index_writer_config(random)?;
    let w = RandomIndexWriter::with_config(random, dir, config);

    for _ in 0..num_docs {
      let mut doc = Document::new();
      for field in 0..num_fields as usize {
        let field_name = format!("int{}", field);
        if random.random_range(0..100) == 17 {
          match field_vector_encodings[field] {
            VectorEncoding::BYTE(_) => {
              let b = Self::random_vector8(random, field_dims[field])?;
              doc.add(KnnByteVectorField::with_similarity_function(
                &field_name,
                b.clone(),
                field_similarity_functions[field],
              )?);
              field_totals[field] += b[0] as f64;
            },
            VectorEncoding::FLOAT32(_) => {
              let v = Self::random_normalized_vector(random, field_dims[field])?;
              doc.add(KnnFloatVectorField::with_similarity_function(
                &field_name,
                v.clone(),
                field_similarity_functions[field],
              )?);
              field_totals[field] += v[0] as f64;
            },
          }
          field_doc_counts[field] += 1;
        }
      }
      w.add_document(random, doc)?;
    }

    let r = w.get_reader(random)?;
    let r = r.get_context()?;
    for field in 0..num_fields as usize {
      let mut doc_count = 0i32;
      let mut checksum = 0f64;
      let field_name = format!("int{}", field);

      match field_vector_encodings[field] {
        VectorEncoding::BYTE(_) => {
          for ctx in r.leaves()? {
            let reader = ctx.reader();
            let byte_vector_values = reader.get_byte_vector_values(&field_name)?;
            if let Some(byte_vector_values) = byte_vector_values {
              doc_count += byte_vector_values.size() as i32;
              let mut iterator = byte_vector_values.iterator()?;
              loop {
                if iterator.next_doc()? == NO_MORE_DOCS {
                  break;
                }
                checksum += byte_vector_values
                  .vector_value(iterator.index()? as usize)?
                  .as_bytes()?[0] as f64;
              }
            }
          }
        },
        VectorEncoding::FLOAT32(_) => {
          for ctx in r.leaves()? {
            let reader = ctx.reader();
            let vector_values = reader.get_float_vector_values(&field_name)?;
            if let Some(vector_values) = vector_values {
              doc_count += vector_values.size() as i32;
              let mut iterator = vector_values.iterator()?;
              loop {
                if iterator.next_doc()? == NO_MORE_DOCS {
                  break;
                }
                checksum += vector_values
                  .vector_value(iterator.index()? as usize)?
                  .as_floats()?[0] as f64;
              }
            }
          }
        },
      }

      assert_eq!(field_doc_counts[field], doc_count);
      let delta = if matches!(field_vector_encodings[field], VectorEncoding::BYTE(_)) {
        num_docs as f64 * 0.01
      } else {
        1e-5
      };
      assert!((field_totals[field] - checksum).abs() <= delta);
    }
    w.close(random)?;
    Ok(())
  }
  fn test_float_vector_scorer_iteration<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let mut iwc = new_index_writer_config(random)?;
    if random.random_bool(0.5) {
      let index_sort =
        Sort::with_fields(vec![SortField::new(Some("sortkey"), SortFieldType::Int)?])?;
      iwc.set_index_sort(index_sort)?;
    }

    let field_name = "field";
    let dir = new_directory_shared(random)?;
    let iw = IndexWriter::new(dir, iwc)?;

    let num_doc = at_least(random, 100);
    let mut dimension = at_least(random, 10) as usize;
    if !dimension.is_multiple_of(2) {
      dimension += 1;
    }

    let similarity = self.random_similarity(random);
    let mut values = vec![None; num_doc as usize];
    for i in 0..num_doc {
      if random.random_range(0..7) != 3 {
        values[i as usize] = Some(Self::random_normalized_vector(random, dimension)?);
      }

      self.add_float(
        random,
        &iw,
        field_name,
        i,
        values[i as usize].clone(),
        similarity,
      )?;

      if random.random_range(0..10) == 2 {
        iw.delete_documents_with_terms(vec![Term::from_text(
          "id",
          random.random_range(0..=i).to_string(),
        )])?;
      }

      if random.random_range(0..10) == 3 {
        iw.commit()?;
      }
    }

    let vector_to_score = Self::random_normalized_vector(random, dimension)?;
    let reader = directory_reader::open_from_writer(&iw)?;
    let reader = reader.get_context()?;

    for ctx in reader.leaves()? {
      let vector_values = ctx.reader().get_float_vector_values(field_name)?;
      let Some(vector_values) = vector_values else {
        continue;
      };

      if vector_values.size() == 0 {
        assert!(vector_values.scorer(vector_to_score.clone())?.is_none());
        continue;
      }

      let scorer = vector_values.scorer(vector_to_score.clone())?;
      assert!(scorer.is_some());
      let mut scorer = scorer.unwrap();

      let mut values_iterator = vector_values.iterator()?;
      while scorer.iterator_mut().next_doc()? != NO_MORE_DOCS {
        if values_iterator.next_doc()? == NO_MORE_DOCS {
          break;
        }
        let score = scorer.score()?;
        assert!(score >= 0f32);
        assert_eq!(scorer.iterator().doc_id(), values_iterator.doc_id());
      }

      let new_scorer = vector_values.scorer(vector_to_score.clone())?;
      assert!(new_scorer.is_some());
      let new_scorer = new_scorer.unwrap();
      assert!(!std::ptr::eq(&scorer, &new_scorer));

      let new_iterator = new_scorer.iterator();
      assert!(!std::ptr::eq(&scorer.iterator(), &new_iterator));
    }

    iw.close()?;
    Ok(())
  }
  fn test_byte_vector_scorer_iteration<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let mut iwc = new_index_writer_config(random)?;
    if random.random_bool(0.5) {
      let index_sort =
        Sort::with_fields(vec![SortField::new(Some("sortkey"), SortFieldType::Int)?])?;
      iwc.set_index_sort(index_sort)?;
    }

    let field_name = "field";
    let dir = new_directory_shared(random)?;
    let iw = IndexWriter::new(dir, iwc)?;

    let num_doc = at_least(random, 100);
    let mut dimension = at_least(random, 10) as usize;
    if !dimension.is_multiple_of(2) {
      dimension += 1;
    }

    let similarity = self.random_similarity(random);
    let mut values = vec![None; num_doc as usize];
    for i in 0..num_doc {
      if random.random_range(0..7) != 3 {
        values[i as usize] = Some(Self::random_vector8(random, dimension)?);
      }

      self.add_byte(
        random,
        &iw,
        field_name,
        i,
        values[i as usize].clone(),
        similarity,
      )?;

      if random.random_range(0..10) == 2 {
        iw.delete_documents_with_terms(vec![Term::from_text(
          "id",
          random.random_range(0..=i).to_string(),
        )])?;
      }

      if random.random_range(0..10) == 3 {
        iw.commit()?;
      }
    }

    let vector_to_score = Self::random_vector8(random, dimension)?;
    let reader = directory_reader::open_from_writer(&iw)?;
    let reader = reader.get_context()?;

    for ctx in reader.leaves()? {
      let vector_values = ctx.reader().get_byte_vector_values(field_name)?;
      let Some(vector_values) = vector_values else {
        continue;
      };

      if vector_values.size() == 0 {
        continue;
      }

      let scorer = vector_values.scorer(vector_to_score.clone())?;
      assert!(scorer.is_some());
      let mut scorer = scorer.unwrap();

      let mut values_iterator = vector_values.iterator()?;
      while scorer.iterator_mut().next_doc()? != NO_MORE_DOCS {
        if values_iterator.next_doc()? == NO_MORE_DOCS {
          break;
        }
        let score = scorer.score()?;
        assert!(score >= 0f32);
        assert_eq!(scorer.iterator().doc_id(), values_iterator.doc_id());
      }

      let new_scorer = vector_values.scorer(vector_to_score.clone())?.unwrap();
      assert!(!std::ptr::eq(&scorer, &new_scorer));
    }

    iw.close()?;
    Ok(())
  }
  fn test_empty_float_vector_data<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let w = IndexWriter::new(dir, new_index_writer_config(random)?)?;

    let mut doc1 = Document::new();
    doc1.add(StringField::from_string("id", "0", Store::No)?);
    doc1.add(KnnFloatVectorField::with_similarity_function(
      "v",
      vec![2.0, 3.0, 5.0, 6.0],
      VectorSimilarityFunction::DotProduct,
    )?);
    w.add_document(doc1)?;

    let mut doc2 = Document::new();
    doc2.add(StringField::from_string("id", "1", Store::No)?);
    w.add_document(doc2)?;

    w.delete_documents_with_terms(vec![Term::from_text("id", "0")])?;
    w.commit()?;
    w.force_merge(1)?;

    let reader = directory_reader::open_from_writer(&w)?;
    let r = get_only_leaf_reader(&reader)?;
    let values = r.get_float_vector_values("v")?;
    assert!(values.is_some());

    let values = values.unwrap();
    assert_eq!(0, values.size());
    assert!(values.scorer(vec![2.0, 3.0, 5.0, 6.0])?.is_none());

    w.close()?;
    Ok(())
  }
  fn test_empty_byte_vector_data<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let w = IndexWriter::new(dir, new_index_writer_config(random)?)?;

    let mut doc1 = Document::new();
    doc1.add(StringField::from_string("id", "0", Store::No)?);
    doc1.add(KnnByteVectorField::with_similarity_function(
      "v",
      vec![2, 3, 5, 6],
      VectorSimilarityFunction::DotProduct,
    )?);
    w.add_document(doc1)?;

    let mut doc2 = Document::new();
    doc2.add(StringField::from_string("id", "1", Store::No)?);
    w.add_document(doc2)?;

    w.delete_documents_with_terms(vec![Term::from_text("id", "0")])?;
    w.commit()?;
    w.force_merge(1)?;

    let reader = directory_reader::open_from_writer(&w)?;
    let r = get_only_leaf_reader(&reader)?;
    let values = r.get_byte_vector_values("v")?;
    assert!(values.is_some());

    let values = values.unwrap();
    assert_eq!(0, values.size());
    assert!(values.scorer(vec![2, 3, 5, 6])?.is_none());

    w.close()?;
    Ok(())
  }
  fn random_similarity<R>(&self, random: &mut R) -> VectorSimilarityFunction
  where
    R: Rng + ?Sized,
  {
    VectorSimilarityFunction::random(random)
  }

  fn random_vector_encoding<R>(&self, random: &mut R) -> VectorEncoding
  where
    R: Rng + ?Sized,
  {
    VectorEncoding::random(random)
  }

  fn test_indexed_value_not_aliased<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let field_name = "field";
    let mut v = vec![0.0f32, 0.0f32];
    let dir = new_directory_shared(random)?;

    let mut iwc = new_index_writer_config(random)?;
    iwc.set_merge_policy(NoMergePolicy::default());
    iwc.set_max_buffered_docs(3);
    iwc.set_ram_buffer_size_mb(-1.0);

    let iw = IndexWriter::new(dir, iwc)?;

    let mut doc1 = Document::new();
    doc1.add(KnnFloatVectorField::with_similarity_function(
      field_name,
      v.clone(),
      VectorSimilarityFunction::Euclidean,
    )?);

    v[0] = 1.0;
    let mut doc2 = Document::new();
    doc2.add(KnnFloatVectorField::with_similarity_function(
      field_name,
      v.clone(),
      VectorSimilarityFunction::Euclidean,
    )?);

    iw.add_document(doc1)?;
    iw.add_document(doc2)?;

    v[0] = 2.0;
    let mut doc3 = Document::new();
    doc3.add(KnnFloatVectorField::with_similarity_function(
      field_name,
      v.clone(),
      VectorSimilarityFunction::Euclidean,
    )?);
    iw.add_document(doc3)?;

    iw.force_merge(1)?;

    let reader = directory_reader::open_from_writer(&iw)?;
    let r = get_only_leaf_reader(&reader)?;
    let vector_values = r.get_float_vector_values(field_name)?.unwrap();

    assert_eq!(3, vector_values.size());
    let mut iterator = vector_values.iterator()?;

    iterator.next_doc()?;
    assert_eq!(0, iterator.index()?);
    assert_eq!(0.0, vector_values.vector_value(0)?.as_floats()?[0]);

    iterator.next_doc()?;
    assert_eq!(1, iterator.index()?);
    assert_eq!(1.0, vector_values.vector_value(1)?.as_floats()?[0]);

    iterator.next_doc()?;
    assert_eq!(2, iterator.index()?);
    assert_eq!(2.0, vector_values.vector_value(2)?.as_floats()?[0]);

    iw.close()?;
    Ok(())
  }

  fn test_sorted_index<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let mut iwc = new_index_writer_config(random)?;
    iwc.set_index_sort(Sort::with_fields(vec![SortField::new(
      Some("sortkey"),
      SortFieldType::Int,
    )?])?)?;

    let field_name = "field";
    let dir = new_directory_shared(random)?;
    let iw = IndexWriter::new(dir, iwc)?;

    self.add_float_default_similarity(&iw, field_name, 1, 1, Some(vec![-1.0, 0.0]))?;
    self.add_float_default_similarity(&iw, field_name, 4, 4, Some(vec![0.0, 1.0]))?;
    self.add_float_default_similarity(&iw, field_name, 3, 3, None)?;
    self.add_float_default_similarity(&iw, field_name, 2, 2, Some(vec![1.0, 0.0]))?;
    iw.force_merge(1)?;

    let reader = directory_reader::open_from_writer(&iw)?;
    let leaf = get_only_leaf_reader(&reader)?;

    let mut stored_fields = leaf.stored_fields()?;
    let vector_values = leaf.get_float_vector_values(field_name)?.unwrap();
    assert_eq!(2, vector_values.dimension());
    assert_eq!(3, vector_values.size());

    let mut iterator = vector_values.iterator()?;
    assert_eq!(
      "1",
      stored_fields
        .document(iterator.next_doc()?)?
        .get("id")?
        .unwrap()
        .as_ref()
    );
    assert_eq!(vec![-1.0, 0.0], vector_values.vector_value(0)?.as_floats()?);

    assert_eq!(
      "2",
      stored_fields
        .document(iterator.next_doc()?)?
        .get("id")?
        .unwrap()
        .as_ref()
    );
    assert_eq!(vec![1.0, 0.0], vector_values.vector_value(1)?.as_floats()?);

    assert_eq!(
      "4",
      stored_fields
        .document(iterator.next_doc()?)?
        .get("id")?
        .unwrap()
        .as_ref()
    );
    assert_eq!(vec![0.0, 1.0], vector_values.vector_value(2)?.as_floats()?);

    assert_eq!(NO_MORE_DOCS, iterator.next_doc()?);

    iw.close()?;
    Ok(())
  }

  fn test_sorted_index_bytes<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let mut iwc = new_index_writer_config(random)?;
    iwc.set_index_sort(Sort::with_fields(vec![SortField::new(
      Some("sortkey"),
      SortFieldType::Int,
    )?])?)?;

    let field_name = "field";
    let dir = new_directory_shared(random)?;
    let iw = IndexWriter::new(dir, iwc)?;

    self.add_byte_default_similarity(&iw, field_name, 1, 1, Some(vec![(-1i8) as u8, 0]))?;
    self.add_byte_default_similarity(&iw, field_name, 4, 4, Some(vec![0, 1]))?;
    self.add_byte_default_similarity(&iw, field_name, 3, 3, None)?;
    self.add_byte_default_similarity(&iw, field_name, 2, 2, Some(vec![1, 0]))?;
    iw.force_merge(1)?;

    let reader = directory_reader::open_from_writer(&iw)?;
    let leaf = get_only_leaf_reader(&reader)?;

    let mut stored_fields = leaf.stored_fields()?;
    let vector_values = leaf.get_byte_vector_values(field_name)?.unwrap();
    assert_eq!(2, vector_values.dimension());
    assert_eq!(3, vector_values.size());

    let mut iterator = vector_values.iterator()?;
    assert_eq!(
      "1",
      stored_fields
        .document(iterator.next_doc()?)?
        .get("id")?
        .unwrap()
        .as_ref()
    );
    assert_eq!(-1, vector_values.vector_value(0)?.as_bytes()?[0] as i8);

    assert_eq!(
      "2",
      stored_fields
        .document(iterator.next_doc()?)?
        .get("id")?
        .unwrap()
        .as_ref()
    );
    assert_eq!(1, vector_values.vector_value(1)?.as_bytes()?[0] as i8);

    assert_eq!(
      "4",
      stored_fields
        .document(iterator.next_doc()?)?
        .get("id")?
        .unwrap()
        .as_ref()
    );
    assert_eq!(0, vector_values.vector_value(2)?.as_bytes()?[0] as i8);

    assert_eq!(NO_MORE_DOCS, iterator.next_doc()?);

    iw.close()?;
    Ok(())
  }

  fn test_index_multiple_knn_vector_fields<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let mut iwc = new_index_writer_config(random)?;
    iwc.set_merge_policy(new_log_merge_policy(random)?);
    let iw = IndexWriter::new(dir, iwc)?;

    let mut doc = Document::new();
    let mut v = vec![1.0, 2.0];
    doc.add(KnnFloatVectorField::with_similarity_function(
      "field1",
      v.clone(),
      VectorSimilarityFunction::Euclidean,
    )?);
    doc.add(KnnFloatVectorField::with_similarity_function(
      "field2",
      vec![1.0, 2.0, 3.0, 4.0],
      VectorSimilarityFunction::Euclidean,
    )?);
    iw.add_document(doc)?;

    v[0] = 2.0;
    let mut doc = Document::new();
    doc.add(KnnFloatVectorField::with_similarity_function(
      "field1",
      v.clone(),
      VectorSimilarityFunction::Euclidean,
    )?);
    doc.add(KnnFloatVectorField::with_similarity_function(
      "field2",
      vec![1.0, 2.0, 3.0, 4.0],
      VectorSimilarityFunction::Euclidean,
    )?);
    iw.add_document(doc)?;

    let mut doc = Document::new();
    doc.add(KnnFloatVectorField::with_similarity_function(
      "field3",
      vec![1.0, 2.0, 3.0, 4.0],
      VectorSimilarityFunction::DotProduct,
    )?);
    iw.add_document(doc)?;

    iw.force_merge(1)?;

    let reader = (directory_reader::open_from_writer(&iw)?).get_context()?;
    let leaf = &reader.leaves()?[0].reader();

    let vector_values = leaf.get_float_vector_values("field1")?.unwrap();
    assert_eq!(2, vector_values.dimension());
    assert_eq!(2, vector_values.size());
    let mut iterator = vector_values.iterator()?;
    iterator.next_doc()?;
    assert_eq!(1.0, vector_values.vector_value(0)?.as_floats()?[0]);
    iterator.next_doc()?;
    assert_eq!(2.0, vector_values.vector_value(1)?.as_floats()?[0]);
    assert_eq!(NO_MORE_DOCS, iterator.next_doc()?);

    let vector_values2 = leaf.get_float_vector_values("field2")?.unwrap();
    let mut it2 = vector_values2.iterator()?;
    assert_eq!(4, vector_values2.dimension());
    assert_eq!(2, vector_values2.size());
    it2.next_doc()?;
    assert_eq!(2.0, vector_values2.vector_value(0)?.as_floats()?[1]);
    it2.next_doc()?;
    assert_eq!(2.0, vector_values2.vector_value(1)?.as_floats()?[1]);
    assert_eq!(NO_MORE_DOCS, it2.next_doc()?);

    let vector_values3 = leaf.get_float_vector_values("field3")?.unwrap();
    assert_eq!(4, vector_values3.dimension());
    assert_eq!(1, vector_values3.size());
    let mut it3 = vector_values3.iterator()?;
    it3.next_doc()?;
    assert!((vector_values3.vector_value(0)?.as_floats()?[0] - 1.0).abs() <= 0.1);
    assert_eq!(NO_MORE_DOCS, it3.next_doc()?);

    iw.close()?;
    Ok(())
  }
  fn test_random<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let mut iwc = new_index_writer_config(random)?;
    if random.random_bool(0.5) {
      iwc.set_index_sort(Sort::with_fields(vec![SortField::new(
        Some("sortkey"),
        SortFieldType::Int,
      )?])?)?;
    }

    let field_name = "field";
    let dir = new_directory_shared(random)?;
    let iw = IndexWriter::new(dir, iwc)?;

    let num_doc = at_least(random, 100);
    let mut dimension = at_least(random, 10) as usize;
    if !dimension.is_multiple_of(2) {
      dimension += 1;
    }

    let mut scratch = vec![0.0f32; dimension];
    let mut num_values = 0i32;
    let similarity = self.random_similarity(random);
    let mut values = vec![None; num_doc as usize];

    for i in 0..num_doc {
      if random.random_range(0..7) != 3 {
        values[i as usize] = Some(Self::random_normalized_vector(random, dimension)?);
        num_values += 1;
      }

      if random.random_bool(0.5) && values[i as usize].is_some() {
        scratch.copy_from_slice(values[i as usize].as_ref().unwrap());
        self.add_float(
          random,
          &iw,
          field_name,
          i,
          Some(scratch.clone()),
          similarity,
        )?;
      } else {
        self.add_float(
          random,
          &iw,
          field_name,
          i,
          values[i as usize].clone(),
          similarity,
        )?;
      }

      if random.random_range(0..10) == 2 {
        let id_to_delete = random.random_range(0..=i);
        iw.delete_documents_with_terms(vec![Term::from_text("id", id_to_delete.to_string())])?;
        if values[id_to_delete as usize].is_some() {
          values[id_to_delete as usize] = None;
          num_values -= 1;
        }
      }

      if random.random_range(0..10) == 3 {
        iw.commit()?;
      }
    }

    let mut num_deletes = 0i32;
    let reader = directory_reader::open_from_writer(&iw)?;
    let reader = reader.get_context()?;
    let mut value_count = 0i32;
    let mut total_size = 0i32;

    for ctx in reader.leaves()? {
      let vector_values = ctx.reader().get_float_vector_values(field_name)?;
      let Some(vector_values) = vector_values else {
        continue;
      };

      total_size += vector_values.size() as i32;
      let mut stored_fields = ctx.reader().stored_fields()?;
      let live_docs = ctx.reader().get_live_docs()?;
      let mut iterator = vector_values.iterator()?;

      loop {
        let doc_id = iterator.next_doc()?;
        if doc_id == NO_MORE_DOCS {
          break;
        }

        let index = iterator.index()? as usize;
        let v = vector_values.vector_value(index)?;
        let v = v.as_floats()?;
        assert_eq!(dimension, v.len());

        let doc = stored_fields.document(doc_id)?;
        let id_string = doc.get_field("id").unwrap().string_value()?;
        let id: usize = id_string.unwrap().parse()?;

        if live_docs
          .as_ref()
          .is_none_or(|bits| bits.get(doc_id as usize).expect(""))
        {
          assert_eq!(values[id].as_ref().unwrap(), &v);
          value_count += 1;
        } else {
          num_deletes += 1;
          assert!(values[id].is_none());
        }
      }
    }

    assert_eq!(num_values, value_count);
    assert_eq!(num_values, total_size - num_deletes);

    iw.close()?;
    Ok(())
  }

  fn test_random_bytes<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let mut iwc = new_index_writer_config(random)?;
    if random.random_bool(0.5) {
      iwc.set_index_sort(Sort::with_fields(vec![SortField::new(
        Some("sortkey"),
        SortFieldType::Int,
      )?])?)?;
    }

    let field_name = "field";
    let dir = new_directory_shared(random)?;
    let iw = IndexWriter::new(dir, iwc)?;

    let num_doc = at_least(random, 100);
    let mut dimension = at_least(random, 10) as usize;
    if !dimension.is_multiple_of(2) {
      dimension += 1;
    }

    let mut scratch = vec![0u8; dimension];
    let mut num_values = 0i32;
    let similarity = self.random_similarity(random);
    let mut values = vec![None; num_doc as usize];

    for i in 0..num_doc {
      if random.random_range(0..7) != 3 {
        values[i as usize] = Some(BytesRef::from_bytes(Self::random_vector8(
          random, dimension,
        )?));
        num_values += 1;
      }

      if random.random_bool(0.5) && values[i as usize].is_some() {
        scratch.copy_from_slice(values[i as usize].as_ref().unwrap().bytes.as_slice());
        self.add_byte(
          random,
          &iw,
          field_name,
          i,
          Some(scratch.clone()),
          similarity,
        )?;
      } else {
        let value = values[i as usize].clone();
        self.add_byte(
          random,
          &iw,
          field_name,
          i,
          match value {
            Some(v) => Some(v.bytes),
            None => None,
          },
          similarity,
        )?;
      }

      if random.random_range(0..10) == 2 {
        let id_to_delete = random.random_range(0..=i);
        iw.delete_documents_with_terms(vec![Term::from_text("id", id_to_delete.to_string())])?;
        if values[id_to_delete as usize].is_some() {
          values[id_to_delete as usize] = None;
          num_values -= 1;
        }
      }

      if random.random_range(0..10) == 3 {
        iw.commit()?;
      }
    }

    let mut num_deletes = 0i32;
    let reader = directory_reader::open_from_writer(&iw)?;
    let reader = reader.get_context()?;
    let mut value_count = 0i32;
    let mut total_size = 0i32;

    for ctx in reader.leaves()? {
      let vector_values = ctx.reader().get_byte_vector_values(field_name)?;
      let Some(vector_values) = vector_values else {
        continue;
      };

      total_size += vector_values.size() as i32;
      let mut stored_fields = ctx.reader().stored_fields()?;
      let live_docs = ctx.reader().get_live_docs()?;
      let mut iterator = vector_values.iterator()?;

      loop {
        let doc_id = iterator.next_doc()?;
        if doc_id == NO_MORE_DOCS {
          break;
        }

        let v = vector_values.vector_value(iterator.index()? as usize)?;
        let v = v.as_bytes()?;
        assert_eq!(dimension, v.len());

        let doc = stored_fields.document(doc_id)?;
        let id_string = doc.get_field("id").unwrap().string_value()?;
        let id: usize = id_string.unwrap().parse()?;

        if live_docs
          .as_ref()
          .is_none_or(|bits| bits.get(doc_id as usize).expect(""))
        {
          assert_eq!(
            0,
            values[id]
              .as_ref()
              .unwrap()
              .cmp(&BytesRef::from_bytes(v.to_vec()))
              .to_int()
          );
          value_count += 1;
        } else {
          num_deletes += 1;
          assert!(values[id].is_none());
        }
      }
    }

    assert_eq!(num_values, value_count);
    assert_eq!(num_values, total_size - num_deletes);

    iw.close()?;
    Ok(())
  }
  fn test_search_with_visited_limit<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let iwc = new_index_writer_config(random)?;
    let field_name = "field";
    let dir = new_directory_shared(random)?;
    let iw = IndexWriter::new(dir, iwc)?;

    let num_doc = 300;
    let dimension = 10usize;
    for i in 0..num_doc {
      let value = if random.random_range(0..7) != 3 {
        Some(Self::random_normalized_vector(random, dimension)?)
      } else {
        None
      };
      self.add_float(
        random,
        &iw,
        field_name,
        i,
        value,
        VectorSimilarityFunction::Euclidean,
      )?;
    }
    iw.force_merge(1)?;

    for _ in 0..30 {
      let id_to_delete = random.random_range(0..num_doc);
      iw.delete_documents_with_terms(vec![Term::from_text("id", id_to_delete.to_string())])?;
    }

    let reader = directory_reader::open_from_writer(&iw)?;
    let reader = reader.get_context()?;
    for ctx in reader.leaves()? {
      let live_docs = ctx.reader().get_live_docs()?;
      let vector_values = ctx.reader().get_float_vector_values(field_name)?;
      let Some(vector_values) = vector_values else {
        continue;
      };

      let mut k = 5 + random.random_range(0..45);
      let mut visited_limit = k + random.random_range(0..5);
      let mut results = ctx.reader().search_nearest_vectors_f32_with_limit(
        field_name,
        Self::random_normalized_vector(random, dimension)?,
        k,
        live_docs.as_ref(),
        visited_limit,
      )?;
      assert_eq!(GreaterThanOrEqualTo, results.total_hits.relation());
      assert_eq!(visited_limit, results.total_hits.value());

      k = vector_values.size();
      visited_limit = k + 30;
      results = ctx.reader().search_nearest_vectors_f32_with_limit(
        field_name,
        Self::random_normalized_vector(random, dimension)?,
        k,
        live_docs.as_ref(),
        visited_limit,
      )?;
      assert_eq!(EqualTo, results.total_hits.relation());
      assert!(results.total_hits.value() <= visited_limit);
    }

    iw.close()?;
    Ok(())
  }
  fn test_random_with_updates_and_graph<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let iwc = new_index_writer_config(random)?;
    let field_name = "field";
    let dir = new_directory_shared(random)?;
    let iw = IndexWriter::new(dir, iwc)?;

    let num_doc = at_least(random, 100);
    let mut dimension = at_least(random, 10) as usize;
    if !dimension.is_multiple_of(2) {
      dimension += 1;
    }

    let mut id2value = vec![None; num_doc as usize];
    for _ in 0..num_doc {
      let id = random.random_range(0..num_doc);
      let value = if random.random_range(0..7) != 3 {
        Some(Self::random_normalized_vector(random, dimension)?)
      } else {
        None
      };
      id2value[id as usize] = value.clone();
      self.add_float(
        random,
        &iw,
        field_name,
        id,
        value,
        VectorSimilarityFunction::Euclidean,
      )?;
    }

    let reader = directory_reader::open_from_writer(&iw)?;
    let reader = reader.get_context()?;
    for ctx in reader.leaves()? {
      let live_docs = ctx.reader().get_live_docs()?;
      let vector_values = ctx.reader().get_float_vector_values(field_name)?;
      let Some(vector_values) = vector_values else {
        continue;
      };

      let mut stored_fields = ctx.reader().stored_fields()?;
      let mut num_live_docs_with_vectors = 0;
      let mut iterator = vector_values.iterator()?;
      loop {
        let doc_id = iterator.next_doc()?;
        if doc_id == NO_MORE_DOCS {
          break;
        }

        let v = vector_values.vector_value(iterator.index()? as usize)?;
        let v = v.as_floats()?;
        assert_eq!(dimension, v.len());

        let document = stored_fields.document(doc_id)?;
        let id_string = document.get_field("id").unwrap().string_value()?;
        let id: usize = id_string.unwrap().parse()?;

        if live_docs
          .as_ref()
          .is_none_or(|bits| bits.get(doc_id as usize).expect(""))
        {
          assert_eq!(
            id2value[id].as_ref().unwrap(),
            &v,
            "values differ for id={}, docid={} leaf={}",
            id,
            doc_id,
            ctx.ord
          );
          num_live_docs_with_vectors += 1;
        } else if let Some(expected) = &id2value[id] {
          assert_ne!(expected, &v);
        }
      }

      if num_live_docs_with_vectors == 0 {
        continue;
      }

      let size = ctx
        .reader()
        .get_float_vector_values(field_name)?
        .unwrap()
        .size();
      let mut k = random.random_range(0..(size / 10 + 1)) + 1;
      if k > num_live_docs_with_vectors {
        k = num_live_docs_with_vectors;
      }

      let results = ctx.reader().search_nearest_vectors_f32_with_limit(
        field_name,
        Self::random_normalized_vector(random, dimension)?,
        k,
        live_docs.as_ref(),
        usize::MAX,
      )?;
      assert_eq!(k.min(size), results.score_docs.len());
      for i in 0..k - 1 {
        assert!(results.score_docs[i].score >= results.score_docs[i + 1].score);
      }
    }

    iw.close()?;
    Ok(())
  }
  fn add_float<R, D>(
    &self,
    random: &mut R,
    iw: &IndexWriter<D>,
    field: &str,
    id: i32,
    vector: Option<Vec<f32>>,
    similarity_function: VectorSimilarityFunction,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
    D: Directory + 'static,
  {
    self.add_float_with_sort_key(
      iw,
      field,
      id,
      random.random_range(0..100),
      vector,
      similarity_function,
    )
  }

  fn add_byte<R, D>(
    &self,
    random: &mut R,
    iw: &IndexWriter<D>,
    field: &str,
    id: i32,
    vector: Option<Vec<u8>>,
    similarity_function: VectorSimilarityFunction,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
    D: Directory + 'static,
  {
    self.add_byte_with_sort_key(
      iw,
      field,
      id,
      random.random_range(0..100),
      vector,
      similarity_function,
    )
  }

  fn add_byte_default_similarity<D>(
    &self,
    iw: &IndexWriter<D>,
    field: &str,
    id: i32,
    sort_key: i32,
    vector: Option<Vec<u8>>,
  ) -> Result<()>
  where
    D: Directory + 'static,
  {
    self.add_byte_with_sort_key(
      iw,
      field,
      id,
      sort_key,
      vector,
      VectorSimilarityFunction::Euclidean,
    )
  }

  fn add_byte_with_sort_key<D>(
    &self,
    iw: &IndexWriter<D>,
    field: &str,
    id: i32,
    sort_key: i32,
    vector: Option<Vec<u8>>,
    similarity_function: VectorSimilarityFunction,
  ) -> Result<()>
  where
    D: Directory + 'static,
  {
    let mut doc = Document::new();
    if let Some(vector) = vector {
      doc.add(KnnByteVectorField::with_similarity_function(
        field,
        vector,
        similarity_function,
      )?);
    }
    doc.add(NumericDocValuesField::new("sortkey", sort_key as i64));
    let id_string = id.to_string();
    doc.add(StringField::from_string(
      "id",
      id_string.clone(),
      Store::Yes,
    )?);
    let id_term = Term::from_text("id", &id_string);
    iw.update_document_with_term(id_term, doc)?;
    Ok(())
  }

  fn add_float_default_similarity<D>(
    &self,
    iw: &IndexWriter<D>,
    field: &str,
    id: i32,
    sort_key: i32,
    vector: Option<Vec<f32>>,
  ) -> Result<()>
  where
    D: Directory + 'static,
  {
    self.add_float_with_sort_key(
      iw,
      field,
      id,
      sort_key,
      vector,
      VectorSimilarityFunction::Euclidean,
    )
  }

  fn add_float_with_sort_key<D>(
    &self,
    iw: &IndexWriter<D>,
    field: &str,
    id: i32,
    sort_key: i32,
    vector: Option<Vec<f32>>,
    similarity_function: VectorSimilarityFunction,
  ) -> Result<()>
  where
    D: Directory + 'static,
  {
    let mut doc = Document::new();
    if let Some(vector) = vector {
      doc.add(KnnFloatVectorField::with_similarity_function(
        field,
        vector,
        similarity_function,
      )?);
    }
    doc.add(NumericDocValuesField::new("sortkey", sort_key as i64));
    let id_string = id.to_string();
    doc.add(StringField::from_string(
      "id",
      id_string.clone(),
      Store::Yes,
    )?);
    let id_term = Term::from_text("id", &id_string);
    iw.update_document_with_term(id_term, doc)?;
    Ok(())
  }
  fn random_vector<R>(random: &mut R, dim: usize) -> Vec<f32>
  where
    R: Rng + ?Sized,
  {
    assert!(dim > 0);
    let mut v = vec![0.0; dim];
    let mut square_sum = 0.0;

    while square_sum == 0.0 {
      square_sum = 0.0;
      for item in &mut v {
        *item = random.random::<f32>();
        square_sum += (*item as f64) * (*item as f64);
      }
    }

    v
  }
  fn random_normalized_vector<R>(random: &mut R, dim: usize) -> Result<Vec<f32>>
  where
    R: Rng + ?Sized,
  {
    let mut v = Self::random_vector(random, dim);
    VectorUtil::l2normalize(&mut v)?;
    Ok(v)
  }

  fn random_vector8<R>(random: &mut R, dim: usize) -> Result<Vec<u8>>
  where
    R: Rng + ?Sized,
  {
    assert!(dim > 0);
    let v = Self::random_normalized_vector(random, dim)?;
    let mut b = vec![0u8; dim];
    for i in 0..dim {
      b[i] = (v[i] * 127.0) as i8 as u8;
    }
    Ok(b)
  }
  fn test_check_index_includes_vectors<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;

    let result = catch_unwind(AssertUnwindSafe(|| -> Result<()> {
      let w = IndexWriter::new(dir.clone(), new_index_writer_config(random)?)?;
      let writer_result = catch_unwind(AssertUnwindSafe(|| -> Result<()> {
        let mut doc = Document::new();
        doc.add(KnnFloatVectorField::with_similarity_function(
          "v1",
          Self::random_normalized_vector(random, 4)?,
          VectorSimilarityFunction::Euclidean,
        )?);
        w.add_document(doc.clone())?;

        doc.add(KnnFloatVectorField::with_similarity_function(
          "v2",
          Self::random_normalized_vector(random, 4)?,
          VectorSimilarityFunction::Euclidean,
        )?);
        w.add_document(doc)?;
        Ok(())
      }));
      let close_result = w.close();
      match writer_result {
        Ok(writer_result) => IOUtils::use_or_suppress_result(writer_result, close_result)?,
        Err(mut payload) => {
          if let Err(close_error) = close_result
            && let Some(error) = payload.downcast_mut::<LuceneError>()
          {
            error.add_suppressed(close_error);
          }
          resume_unwind(payload)
        },
      }

      let mut output = Vec::new();
      let status = TestUtil::check_index_with_options(
        random,
        dir.clone(),
        Level::MIN_LEVEL_FOR_INTEGRITY_CHECKS,
        true,
        true,
        Some(&mut output),
      )?;
      assert_eq!(1, status.segment_infos.len());
      let seg_status = &status.segment_infos[0];
      let vector_values_status = seg_status
        .vector_values_status
        .as_ref()
        .expect("vector values status");
      // total 3 vector values were indexed:
      assert_eq!(3, vector_values_status.total_vector_values);
      // ... across 2 fields:
      assert_eq!(2, vector_values_status.total_knn_vector_fields);

      // Make sure CheckIndex in fact declares that it is testing vectors!
      assert!(String::from_utf8_lossy(&output).contains("test: vectors..."));
      Ok(())
    }));
    let close_result = dir.as_ref().close();
    match result {
      Ok(result) => IOUtils::use_or_suppress_result(result, close_result),
      Err(mut payload) => {
        if let Err(close_error) = close_result
          && let Some(error) = payload.downcast_mut::<LuceneError>()
        {
          error.add_suppressed(close_error);
        }
        resume_unwind(payload)
      },
    }
  }
  fn test_similarity_function_identifiers(&self) -> Result<()> {
    assert_eq!(0, VectorSimilarityFunction::Euclidean as usize);
    assert_eq!(1, VectorSimilarityFunction::DotProduct as usize);
    assert_eq!(2, VectorSimilarityFunction::Cosine as usize);
    assert_eq!(3, VectorSimilarityFunction::MaximumInnerProduct as usize);
    assert_eq!(4, VectorSimilarityFunction::COUNT);
    Ok(())
  }

  fn test_vector_encoding_ordinals(&self) -> Result<()> {
    assert_eq!(0, VectorEncoding::BYTE(0).ordinal());
    assert_eq!(1, VectorEncoding::FLOAT32(0).ordinal());
    assert_eq!(2, VectorEncoding::COUNT);
    Ok(())
  }

  fn test_advance<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;

    {
      let w = IndexWriter::new(dir.clone(), new_index_writer_config(random)?)?;
      let numdocs = at_least(random, 1500);
      let field_name = "field";

      for _ in 0..numdocs {
        let mut doc = Document::new();
        if random.random_range(0..4) == 3 {
          doc.add(KnnFloatVectorField::with_similarity_function(
            field_name,
            vec![0.0; 4],
            VectorSimilarityFunction::Euclidean,
          )?);
        }
        w.add_document(doc)?;
      }

      w.force_merge(1)?;

      let reader = directory_reader::open_from_writer(&w)?;
      let r = get_only_leaf_reader(&reader)?;
      let mut vector_values = r.get_float_vector_values(field_name)?.unwrap();
      let mut vector_docs = vec![0; vector_values.size() + 1];
      let mut cur = -1;
      let mut iterator = vector_values.iterator()?;
      while (cur + 1) < vector_values.size() as i32 + 1 {
        cur += 1;
        vector_docs[cur as usize] = iterator.next_doc()?;
        if cur != 0 {
          assert!(vector_docs[cur as usize] > vector_docs[(cur - 1) as usize]);
        }
      }

      vector_values = r.get_float_vector_values(field_name)?.unwrap();
      let mut iter = vector_values.iterator()?;
      cur = -1;
      let mut i = 0;
      while i < numdocs {
        if random.random_range(0..4) == 3 {
          loop {
            cur += 1;
            if vector_docs[cur as usize] >= i {
              break;
            }
          }
          assert_eq!(vector_docs[cur as usize], iter.advance(i)?);
          assert_eq!(vector_docs[cur as usize], iter.doc_id());
          if iter.doc_id() == NO_MORE_DOCS {
            break;
          }
          i = iter.doc_id();
        }
        i += 1;
      }

      w.close()?;
    }

    Ok(())
  }

  fn test_vector_values_report_correct_docs<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_docs = at_least(random, 1000);
    let mut dim = random.random_range(1..=20) as usize;
    if !dim.is_multiple_of(2) {
      dim += 1;
    }
    let vector_encoding = self.random_vector_encoding(random);
    let similarity_function = self.random_similarity(random);
    let mut field_values_check_sum = 0f64;
    let mut field_doc_count = 0usize;
    let mut field_sum_doc_ids = 0i64;

    let dir = new_directory_shared(random)?;
    let config = new_index_writer_config(random)?;
    let w = RandomIndexWriter::with_config(random, dir, config);

    for _ in 0..num_docs {
      let mut doc = Document::new();
      let doc_id = random.random_range(0..num_docs);
      doc.add(StringField::from_string(
        "id",
        doc_id.to_string(),
        Store::Yes,
      )?);
      if random.random_range(0..4) == 3 {
        match vector_encoding {
          VectorEncoding::BYTE(_) => {
            let b = Self::random_vector8(random, dim)?;
            field_values_check_sum += (b[0] as i8) as f64;
            doc.add(KnnByteVectorField::with_similarity_function(
              "knn_vector",
              b,
              similarity_function,
            )?);
          },
          VectorEncoding::FLOAT32(_) => {
            let v = Self::random_normalized_vector(random, dim)?;
            field_values_check_sum += v[0] as f64;
            doc.add(KnnFloatVectorField::with_similarity_function(
              "knn_vector",
              v,
              similarity_function,
            )?);
          },
        }
        field_doc_count += 1;
        field_sum_doc_ids += doc_id as i64;
      }
      w.add_document(random, doc)?;
    }

    if random.random_bool(0.5) {
      w.force_merge(random, 1)?;
    }

    let reader = w.get_reader(random)?;
    let reader = reader.get_context()?;
    let mut checksum = 0f64;
    let mut doc_count = 0usize;
    let mut sum_doc_ids = 0i64;
    let mut sum_ord_to_doc_ids = 0i64;

    match vector_encoding {
      VectorEncoding::BYTE(_) => {
        for ctx in reader.leaves()? {
          let byte_vector_values = ctx.reader().get_byte_vector_values("knn_vector")?;
          let Some(byte_vector_values) = byte_vector_values else {
            continue;
          };

          doc_count += byte_vector_values.size();
          let mut stored_fields = ctx.reader().stored_fields()?;
          let mut iter = byte_vector_values.iterator()?;
          loop {
            let leaf_doc_id = iter.next_doc()?;
            if leaf_doc_id == NO_MORE_DOCS {
              break;
            }

            let ord = iter.index()? as usize;
            checksum += (byte_vector_values.vector_value(ord)?.as_bytes()?[0] as i8) as f64;
            let doc = stored_fields.document(leaf_doc_id)?;
            let id = doc
              .get_field("id")
              .unwrap()
              .string_value()?
              .unwrap()
              .parse::<i64>()?;
            sum_doc_ids += id;
          }

          for ord in 0..byte_vector_values.size() {
            let doc = stored_fields.document(byte_vector_values.ord_to_doc(ord)? as i32)?;
            let id = doc
              .get_field("id")
              .unwrap()
              .string_value()?
              .unwrap()
              .parse::<i64>()?;
            sum_ord_to_doc_ids += id;
          }
        }
      },
      VectorEncoding::FLOAT32(_) => {
        for ctx in reader.leaves()? {
          let vector_values = ctx.reader().get_float_vector_values("knn_vector")?;
          let Some(vector_values) = vector_values else {
            continue;
          };

          doc_count += vector_values.size();
          let mut stored_fields = ctx.reader().stored_fields()?;
          let mut iter = vector_values.iterator()?;
          loop {
            let leaf_doc_id = iter.next_doc()?;
            if leaf_doc_id == NO_MORE_DOCS {
              break;
            }

            let ord = iter.index()? as usize;
            checksum += vector_values.vector_value(ord)?.as_floats()?[0] as f64;
            let doc = stored_fields.document(leaf_doc_id)?;
            let id = doc
              .get_field("id")
              .unwrap()
              .string_value()?
              .unwrap()
              .parse::<i64>()?;
            sum_doc_ids += id;
          }

          for ord in 0..vector_values.size() {
            let doc = stored_fields.document(vector_values.ord_to_doc(ord)? as i32)?;
            let id = doc
              .get_field("id")
              .unwrap()
              .string_value()?
              .unwrap()
              .parse::<i64>()?;
            sum_ord_to_doc_ids += id;
          }
        }
      },
    }

    let delta = if matches!(vector_encoding, VectorEncoding::BYTE(_)) {
      num_docs as f64 * 0.2
    } else {
      1e-5
    };
    assert!(
      (field_values_check_sum - checksum).abs() <= delta,
      "encoding={vector_encoding:?}, expected checksum={}, actual checksum={}",
      field_values_check_sum,
      checksum
    );
    assert_eq!(field_doc_count, doc_count);
    assert_eq!(field_sum_doc_ids, sum_doc_ids);
    assert_eq!(field_sum_doc_ids, sum_ord_to_doc_ids);

    w.close(random)?;
    Ok(())
  }

  fn test_mismatched_fields<R>(&self, _random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    // TODO MismatchedCodecReader未实现
    Ok(())
  }
}
pub struct TestMergeScheduler {
  ex: Arc<AtomicBool>,
}

impl TestMergeScheduler {
  pub(crate) fn new(ex: Arc<AtomicBool>) -> Self {
    Self { ex }
  }
}

impl CloseableRef for TestMergeScheduler {}

impl MergeScheduler for TestMergeScheduler {
  fn merge<MS, D>(&self, merge_source: MS, _trigger: MergeTrigger) -> Result<()>
  where
    MS: MergeSource<D> + Clone + 'static,
    D: Directory + 'static,
    OneMerge<D, MS::Reader>: Send + 'static,
  {
    while let Some(merge) = merge_source.get_next_merge()? {
      let result: Result<()> = merge_source.merge(merge);
      if result.is_err() {
        self.ex.store(true, Ordering::SeqCst);
        return result;
      }
    }
    Ok(())
  }

  type Directory<D>
    = D
  where
    D: Directory;

  fn wrap_for_merge<D>(&self, in_: D) -> Result<Self::Directory<D>>
  where
    D: Directory,
  {
    Ok(in_)
  }
}
