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
use crate::core::analysis::token_attributes::payload_attribute::PayloadAttribute;
use crate::core::document::binary_point::BinaryPoint;
use crate::core::document::document::Document;
use crate::core::document::field::{Field, Store};
use crate::core::document::field_type::FieldType;
use crate::core::document::fields::FieldTokenStreamEnum;
use crate::core::document::knn_float_vector_field::KnnFloatVectorField;
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::document::stored_field::StoredField;
use crate::core::document::string_field::StringField;
use crate::core::document::text_field::TYPE_NOT_STORED;
use crate::core::index::BytesRef;
use crate::core::index::check_index::{CheckIndex, Level};
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::no_merge_policy::NoMergePolicy;
use crate::core::index::soft_deletes_retention_merge_policy::SoftDeletesRetentionMergePolicy;
use crate::core::index::term::Term;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::search::match_all_docs_query::MatchAllDocsQuery;
use crate::core::search::sort::Sort;
use crate::core::search::sort_field::{SortField, SortFieldType};
use crate::core::store::directory::{DirEnum, Directory};
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, LuceneErrorKind, Result};
use crate::core::util::io_utils::IOUtils;
use crate::core::util::numeric_utils::NumericUtils;
use crate::core::util::vector_util::VectorUtil;
use crate::test_framework::core::analysis::{canned_token_stream::CannedTokenStream, token};
use crate::test_framework::core::index::base_test_check_index::BaseTestCheckIndex;
use crate::test_framework::core::index::test_check_index::DeleteNothingIndexDeletionPolicy;
use crate::test_framework::core::util::lucene_test_case::{
  new_directory_shared, new_index_writer_config, new_mock_directory, random, slow_file_exists,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::prelude::StdRng;
use rand::{Rng, RngExt};
use std::io::Sink;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::Arc;

#[allow(dead_code)] // for quick search
pub struct TestCheckIndex;

impl BaseTestCheckIndex for TestCheckIndex {}

fn run_base_case<F>(f: F) -> Result<()>
where
  F: FnOnce(&TestCheckIndex, &mut StdRng, &Arc<DirEnum>) -> Result<()>,
{
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let result = catch_unwind(AssertUnwindSafe(|| f(&TestCheckIndex, &mut random, &dir)));
  let close_result = dir.close();
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

mod base_test_check_index_test {
  use super::run_base_case;
  use crate::core::util::error::lucene_error::Result;
  use crate::test_framework::core::index::base_test_check_index::BaseTestCheckIndex;

  #[test]
  fn test_deleted_docs() -> Result<()> {
    run_base_case(|case, random, dir| case.test_deleted_docs(random, dir))
  }

  #[test]
  fn test_checksums_only() -> Result<()> {
    run_base_case(|case, random, dir| case.test_checksums_only(random, dir))
  }

  #[test]
  fn test_checksums_only_verbose() -> Result<()> {
    run_base_case(|case, random, dir| case.test_checksums_only_verbose(random, dir))
  }

  #[test]
  fn test_obtains_lock() -> Result<()> {
    run_base_case(|case, _random, dir| case.test_obtains_lock(dir))
  }
}

#[test]
fn test_check_index_all_valid() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let result = catch_unwind(AssertUnwindSafe(|| -> Result<()> {
    let live_doc_count = 1 + random.random_range(0..10);
    let mut config = new_index_writer_config(&mut random)?;
    config.set_index_sort(Sort::with_fields(vec![SortField::with_reverse(
      Some("sort_field"),
      SortFieldType::Int,
      true,
    )?])?)?;
    config.set_soft_deletes_field("soft_delete");
    // preserves soft-deletes across merges
    config.set_merge_policy(SoftDeletesRetentionMergePolicy::new(
      "soft_delete",
      || Ok(MatchAllDocsQuery::new().into()),
      config.get_merge_policy().as_ref().clone(),
    ));

    let w = IndexWriter::new(Arc::clone(&dir), config)?;
    let writer_result = catch_unwind(AssertUnwindSafe(|| -> Result<()> {
      for _ in 0..live_doc_count {
        let mut doc = Document::new();

        // stored field
        doc.add(StringField::from_string(
          "id",
          random.random::<i32>().to_string(),
          Store::Yes,
        )?);
        doc.add(StoredField::from_string(
          "field",
          format!("value{}", TestUtil::random_simple_string(&mut random)),
        )?);

        // vector
        doc.add(KnnFloatVectorField::new(
          "v1",
          random_vector(&mut random, 3)?,
        )?);
        doc.add(KnnFloatVectorField::new(
          "v2",
          random_vector(&mut random, 3)?,
        )?);

        // doc value
        doc.add(NumericDocValuesField::new("dv", random.random()));

        // doc value with skip index
        doc.add(NumericDocValuesField::indexed_field(
          "dv_skip",
          random.random(),
        ));

        // point value
        let mut point = [0_u8; 4];
        NumericUtils::int_to_sortable_bytes(random.random(), &mut point, 0);
        doc.add(BinaryPoint::new("point", [point])?);

        // term vector
        let mut token1 = token::with_range(Some("bar"), 0, 3)?;
        token1
          .sub
          .token
          .set_payload(Some(BytesRef::from_string("pay1")));
        let mut token2 = token::with_range(Some("bar"), 4, 8)?;
        token2
          .sub
          .token
          .set_payload(Some(BytesRef::from_string("pay2")));
        let mut ft = FieldType::from_ref(&*TYPE_NOT_STORED)?;
        ft.set_store_term_vectors(true)?;
        ft.set_store_term_vector_positions(true)?;
        ft.set_store_term_vector_payloads(true)?;
        doc.add(Field::from_token_stream(
          "termvector",
          FieldTokenStreamEnum::custom(CannedTokenStream::new(vec![token1, token2])),
          ft,
        )?);

        w.add_document(doc)?;
      }

      let mut tombstone = Document::new();
      tombstone.add(NumericDocValuesField::new("soft_delete", 1));
      w.soft_update_document(
        Term::from_text("id", "1"),
        tombstone,
        vec![NumericDocValuesField::new("soft_delete", 1).into()],
      )?;
      w.force_merge(1)
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

    let mut output = Vec::with_capacity(1024);
    let status = TestUtil::check_index_with_options(
      &mut random,
      Arc::clone(&dir),
      Level::MIN_LEVEL_FOR_INTEGRITY_CHECKS,
      true,
      true,
      Some(&mut output),
    )?;

    assert_eq!(1, status.segment_infos.len());

    let seg_status = &status.segment_infos[0];
    let output = String::from_utf8_lossy(&output);

    // confirm live docs testing status
    let live_doc_status = seg_status
      .live_doc_status
      .as_ref()
      .expect("live doc status");
    assert_eq!(0, live_doc_status.num_deleted);
    assert!(output.contains("test: check live docs"));
    assert!(live_doc_status.error.is_none());

    // confirm field infos testing status
    let field_info_status = seg_status
      .field_info_status
      .as_ref()
      .expect("field info status");
    assert_eq!(9, field_info_status.tot_fields);
    assert!(output.contains("test: field infos"));
    assert!(field_info_status.error.is_none());

    // confirm field norm (from term vector) testing status
    let field_norm_status = seg_status
      .field_norm_status
      .as_ref()
      .expect("field norm status");
    assert_eq!(1, field_norm_status.tot_fields);
    assert!(output.contains("test: field norms"));
    assert!(field_norm_status.error.is_none());

    // confirm term index testing status
    let term_index_status = seg_status
      .term_index_status
      .as_ref()
      .expect("term index status");
    assert!(term_index_status.term_count > 0);
    assert!(term_index_status.tot_freq > 0);
    assert!(term_index_status.tot_pos > 0);
    assert!(output.contains("test: terms, freq, prox"));
    assert!(term_index_status.error.is_none());

    // confirm stored field testing status
    // add storedField from tombstone doc
    let stored_field_status = seg_status
      .stored_field_status
      .as_ref()
      .expect("stored field status");
    assert_eq!(live_doc_count + 1, stored_field_status.doc_count);
    assert_eq!(
      i64::from(2 * live_doc_count),
      stored_field_status.tot_fields
    );
    assert!(output.contains("test: stored fields"));
    assert!(stored_field_status.error.is_none());

    // confirm term vector testing status
    let term_vector_status = seg_status
      .term_vector_status
      .as_ref()
      .expect("term vector status");
    assert_eq!(live_doc_count, term_vector_status.doc_count);
    assert_eq!(i64::from(live_doc_count), term_vector_status.tot_vectors);
    assert!(output.contains("test: term vectors"));
    assert!(term_vector_status.error.is_none());

    // confirm doc values testing status
    let doc_values_status = seg_status
      .doc_values_status
      .as_ref()
      .expect("doc values status");
    assert_eq!(3, doc_values_status.total_numeric_fields);
    assert_eq!(1, doc_values_status.total_skipping_index);
    assert!(output.contains("test: docvalues"));
    assert!(doc_values_status.error.is_none());

    // confirm point values testing status
    let points_status = seg_status.points_status.as_ref().expect("points status");
    assert_eq!(1, points_status.total_value_fields);
    assert_eq!(live_doc_count as i64, points_status.total_value_points);
    assert!(output.contains("test: points"));
    assert!(points_status.error.is_none());

    // confirm vector testing status
    let vector_values_status = seg_status
      .vector_values_status
      .as_ref()
      .expect("vector values status");
    assert_eq!(
      i64::from(2 * live_doc_count),
      vector_values_status.total_vector_values
    );
    assert_eq!(2, vector_values_status.total_knn_vector_fields);
    assert!(output.contains("test: vectors"));
    assert!(vector_values_status.error.is_none());

    // confirm index sort testing status
    assert!(output.contains("test: index sort"));
    assert!(
      seg_status
        .index_sort_status
        .as_ref()
        .expect("index sort status")
        .error
        .is_none()
    );

    // confirm soft deletes testing status
    assert!(output.contains("test: check soft deletes"));
    assert!(
      seg_status
        .soft_deletes_status
        .as_ref()
        .expect("soft deletes status")
        .error
        .is_none()
    );
    Ok(())
  }));

  let close_result = dir.close();
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

#[test]
fn test_invalid_thread_count_argument() {
  let args = vec!["-threadCount".to_string(), "0".to_string()];
  assert!(matches!(
    (CheckIndex::parse_options(&args))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
}

fn random_vector<R>(random: &mut R, dim: usize) -> Result<Vec<f32>>
where
  R: Rng + ?Sized,
{
  let mut v = vec![0.0; dim];
  for value in &mut v {
    *value = random.random();
  }
  VectorUtil::l2normalize(&mut v)?;
  Ok(v)
}

// https://github.com/apache/lucene/issues/7820 -- when the most recent commit point in
// the index is OK, but older commit points are broken, CheckIndex fails to detect and
// correct that, while opening an IndexWriter on the index will fail since IndexWriter
// loads all commit points on init
#[test]
fn test_prior_broken_commit_point() -> Result<()> {
  let mut random = random();
  let dir = Arc::new(new_mock_directory(&mut random)?);

  // disable this normally useful test infra feature since this test intentionally leaves broken
  // indices:
  dir.set_check_index_on_close(false);

  let result = catch_unwind(AssertUnwindSafe(|| -> Result<()> {
    let mut iwc = IndexWriterConfig::new()?;
    iwc
      .set_merge_policy(NoMergePolicy::default())
      .set_index_deletion_policy(DeleteNothingIndexDeletionPolicy);

    let iw = IndexWriter::new(Arc::clone(&dir), iwc)?;
    let writer_result = catch_unwind(AssertUnwindSafe(|| -> Result<()> {
      // create first segment, and commit point referencing only segment 0
      let mut doc = Document::new();
      doc.add(
        crate::core::document::string_field::StringField::from_string(
          "id",
          "a",
          crate::core::document::field::Store::No,
        )?,
      );
      iw.add_document(&mut doc)?;
      iw.commit()?;

      // NOTE: we are (illegally) relying on precise file naming here -- if Codec or IW's
      // behaviour changes, this may need fixing:
      assert!(slow_file_exists(dir.as_ref(), "_0.si")?);

      // create second segment, and another commit point referencing only segment 1
      doc.add(
        crate::core::document::string_field::StringField::from_string(
          "id",
          "a",
          crate::core::document::field::Store::No,
        )?,
      );
      iw.update_document_with_term(Term::from_text("id", "a"), &mut doc)?;
      iw.commit()?;

      // NOTE: we are (illegally) relying on precise file naming here -- if Codec or IW's
      // behaviour changes, this may need fixing:
      assert!(slow_file_exists(dir.as_ref(), "_0.si")?);
      assert!(slow_file_exists(dir.as_ref(), "_1.si")?);
      Ok(())
    }));
    let close_result = iw.close();
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

    let mut checkers = CheckIndex::<_, _, Sink>::new(Arc::clone(&dir))?;
    let checker_result = catch_unwind(AssertUnwindSafe(|| -> Result<()> {
      let check_index_status = checkers.check_index()?;
      assert!(check_index_status.clean);
      Ok(())
    }));
    let close_result = checkers.close();
    match checker_result {
      Ok(checker_result) => IOUtils::use_or_suppress_result(checker_result, close_result)?,
      Err(mut payload) => {
        if let Err(close_error) = close_result
          && let Some(error) = payload.downcast_mut::<LuceneError>()
        {
          error.add_suppressed(close_error);
        }
        resume_unwind(payload)
      },
    }

    // now corrupt segment 0, which is referenced by only the first commit point, by removing its
    // .si file (_0.si)
    dir.delete_file("_0.si")?;

    let mut checkers = CheckIndex::<_, _, Sink>::new(Arc::clone(&dir))?;
    let checker_result = catch_unwind(AssertUnwindSafe(|| -> Result<()> {
      let check_index_status = checkers.check_index()?;
      assert!(!check_index_status.clean);
      Ok(())
    }));
    let close_result = checkers.close();
    match checker_result {
      Ok(checker_result) => IOUtils::use_or_suppress_result(checker_result, close_result),
      Err(mut payload) => {
        if let Err(close_error) = close_result
          && let Some(error) = payload.downcast_mut::<LuceneError>()
        {
          error.add_suppressed(close_error);
        }
        resume_unwind(payload)
      },
    }
  }));

  let close_result = dir.close();
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
