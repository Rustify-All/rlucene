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
use crate::core::codecs::Codecs;
use crate::core::codecs::lucene101_codec::{Lucene101Codec, Mode};
use crate::core::document::document::Document;
use crate::core::document::stored_field::StoredField;
use crate::core::index::directory_reader;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::store::directory::DirEnum;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::index::base_index_file_format_test_case::BaseIndexFileFormatTestCase;
use crate::test_framework::core::index::base_stored_fields_format_test_case::BaseStoredFieldsFormatTestCase;
use crate::test_framework::core::util::lucene_test_case::{
  new_directory_shared, new_index_writer_config, random,
};
use rand::prelude::StdRng;
use rand::{Rng, RngExt};
use std::sync::Arc;

#[allow(dead_code)] // for quick search
struct TestLucene90StoredFieldsFormatHighCompression;

impl BaseIndexFileFormatTestCase for TestLucene90StoredFieldsFormatHighCompression {
  type Defaults = crate::test_framework::core::index::base_stored_fields_format_test_case::BaseStoredFieldsFormatTestCaseDefaults;

  fn get_codec(&self) -> Result<Codecs> {
    Ok(Lucene101Codec::with_mode(Mode::BestCompression).into())
  }
}

impl BaseStoredFieldsFormatTestCase for TestLucene90StoredFieldsFormatHighCompression {}

impl TestLucene90StoredFieldsFormatHighCompression {
  /// Change compression params (leaving it the same for old segments) and tests that nothing breaks.
  fn test_mixed_compressions<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    for _ in 0..10 {
      let mut iwc: IndexWriterConfig<Arc<DirEnum>> = new_index_writer_config(random)?;
      iwc.set_codec(Lucene101Codec::with_mode(if random.random_bool(0.5) {
        Mode::BestSpeed
      } else {
        Mode::BestCompression
      }));
      let writer = IndexWriter::new(dir.clone(), new_index_writer_config(random)?)?;
      let mut doc = Document::new();
      doc.add(StoredField::from_string("field1", "value1")?);
      doc.add(StoredField::from_string("field2", "value2")?);
      writer.add_document(doc)?;
      if random.random_range(0..4) == 0 {
        writer.force_merge(1)?;
      }
      writer.commit()?;
      writer.close()?;
    }

    let reader = directory_reader::open(dir.clone())?;
    assert_eq!(10, reader.num_docs()?);
    let mut stored_fields = reader.stored_fields()?;
    for i in 0..10 {
      let doc = stored_fields.document(i)?;
      assert_eq!(
        "value1",
        doc.get("field1")?.expect("field1 should exist").as_ref()
      );
      assert_eq!(
        "value2",
        doc.get("field2")?.expect("field2 should exist").as_ref()
      );
    }
    reader.close()?;
    // checkindex
    dir.close()?;
    Ok(())
  }
}

fn run_case<F>(f: F) -> Result<()>
where
  F: FnOnce(&TestLucene90StoredFieldsFormatHighCompression, &mut StdRng) -> Result<()>,
{
  let mut random = random();
  let case = TestLucene90StoredFieldsFormatHighCompression;
  f(&case, &mut random)
}

#[test]
fn test_mixed_compressions() -> Result<()> {
  run_case(|case, random| case.test_mixed_compressions(random))
}

mod base_stored_fields_format_test_case_test {
  use super::run_case;
  use crate::core::util::error::lucene_error::Result;
  use crate::test_framework::core::index::base_stored_fields_format_test_case::BaseStoredFieldsFormatTestCase;

  #[test]
  fn test_random_stored_fields() -> Result<()> {
    run_case(|case, random| case.test_random_stored_fields(random))
  }

  #[test]
  fn test_stored_fields_order() -> Result<()> {
    run_case(|case, random| case.test_stored_fields_order(random))
  }

  #[test]
  fn test_binary_field_offset_length() -> Result<()> {
    run_case(|case, random| case.test_binary_field_offset_length(random))
  }

  #[test]
  fn test_numeric_field() -> Result<()> {
    run_case(|case, random| case.test_numeric_field(random))
  }

  #[test]
  fn test_indexed_bit() -> Result<()> {
    run_case(|case, random| case.test_indexed_bit(random))
  }

  #[test]
  fn test_read_skip() -> Result<()> {
    run_case(|case, random| case.test_read_skip(random))
  }

  #[test]
  fn test_empty_docs() -> Result<()> {
    run_case(|case, random| case.test_empty_docs(random))
  }

  #[test]
  fn test_concurrent_reads() -> Result<()> {
    run_case(|case, random| case.test_concurrent_reads(random))
  }

  #[test]
  fn test_write_read_merge() -> Result<()> {
    run_case(|case, random| case.test_write_read_merge(random))
  }

  #[test]
  fn test_merge_filter_reader() -> Result<()> {
    run_case(|case, random| case.test_merge_filter_reader(random))
  }

  #[cfg(feature = "nightly")]
  #[test]
  #[ignore = "nightly"]
  fn test_big_documents() -> Result<()> {
    run_case(|case, random| case.test_big_documents(random))
  }

  #[test]
  fn test_bulk_merge_with_deletes() -> Result<()> {
    run_case(|case, random| case.test_bulk_merge_with_deletes(random))
  }

  #[test]
  fn test_mismatched_fields() -> Result<()> {
    run_case(|case, random| case.test_mismatched_fields(random))
  }

  #[test]
  fn test_random_stored_fields_with_index_sort() -> Result<()> {
    run_case(|case, random| case.test_random_stored_fields_with_index_sort(random))
  }

  #[test]
  fn test_line_file_docs() -> Result<()> {
    run_case(|case, random| case.test_line_file_docs(random))
  }
}

mod base_index_file_format_test_case_test {
  use super::run_case;
  use crate::core::util::error::lucene_error::Result;
  use crate::test_framework::core::index::base_index_file_format_test_case::BaseIndexFileFormatTestCase;

  #[test]
  fn test_merge_stability() -> Result<()> {
    run_case(|case, random| case.test_merge_stability(random))
  }

  #[test]
  fn test_multi_close() -> Result<()> {
    run_case(|case, random| case.test_multi_close(random))
  }

  #[test]
  fn test_random_exceptions() -> Result<()> {
    run_case(|case, random| case.test_random_exceptions(random))
  }

  #[test]
  fn test_check_integrity_reads_all_bytes() -> Result<()> {
    run_case(|case, random| case.test_check_integrity_reads_all_bytes(random))
  }
}
