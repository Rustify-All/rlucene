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
use crate::core::codecs::lucene90::lucene90_doc_values_format::Lucene90DocValuesFormat;
use crate::core::document::document::Document;
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::document::sorted_numeric_doc_values_field::SortedNumericDocValuesField;
use crate::core::index::doc_values_skipper::DocValuesSkipper;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::sort::Sort;
use crate::core::search::sort_field::{SortField, SortFieldType};
use crate::core::store::directory::Directory;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::test_framework::core::index::base_doc_values_format_test_case::BaseDocValuesFormatTestCase;
use crate::test_framework::core::index::base_index_file_format_test_case::BaseIndexFileFormatTestCase;
use crate::test_framework::core::index::legacy_base_doc_values_format_test_case::LegacyBaseDocValuesFormatTestCase;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, get_only_leaf_reader, new_directory_shared, random,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::prelude::StdRng;
use rand::{Rng, RngExt};

/// Tests `Lucene90DocValuesFormat` with a custom skipper interval size.
#[allow(dead_code)] // for quick search
struct TestLucene90DocValuesFormatVariableSkipInterval {
  skip_index_interval_size: i32,
}

impl BaseIndexFileFormatTestCase for TestLucene90DocValuesFormatVariableSkipInterval {
  type Defaults = crate::test_framework::core::index::legacy_base_doc_values_format_test_case::LegacyBaseDocValuesFormatTestCaseDefaults;

  fn get_codec(&self) -> Result<Codecs> {
    Ok(
      TestUtil::always_doc_values_format(Lucene90DocValuesFormat::with_skip_index_interval_size(
        self.skip_index_interval_size,
      )?)
      .into(),
    )
  }
}

impl LegacyBaseDocValuesFormatTestCase for TestLucene90DocValuesFormatVariableSkipInterval {}
impl BaseDocValuesFormatTestCase for TestLucene90DocValuesFormatVariableSkipInterval {}

impl TestLucene90DocValuesFormatVariableSkipInterval {
  fn test_skip_index_interval_size<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let interval_size = random.random_range(i32::MIN..2);
    let error = match Lucene90DocValuesFormat::with_skip_index_interval_size(interval_size) {
      Ok(_) => panic!("expected an invalid skip-index interval size"),
      Err(error) => error,
    };
    assert!(matches!(error, LuceneError::IllegalArgument(_)));
    assert!(
      error
        .to_string()
        .contains("skip_index_interval_size must be > 1")
    );
    Ok(())
  }

  fn test_skipper_all_equal_value<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let mut config = IndexWriterConfig::new()?;
    config.set_codec(self.get_codec()?);
    let directory = new_directory_shared(random)?;
    let writer = RandomIndexWriter::with_config(random, directory.clone(), config);
    let num_docs = at_least(random, 100);
    for _ in 0..num_docs {
      let mut document = Document::new();
      document.add(NumericDocValuesField::indexed_field("dv", 0));
      writer.add_document(random, document)?;
    }
    writer.force_merge(random, 1)?;

    let reader = writer.get_reader(random)?;
    let leaf = get_only_leaf_reader(&reader)?;
    let mut skipper = leaf.get_doc_values_skipper("dv")?.unwrap();
    skipper.advance(0)?;
    assert_eq!(0, skipper.min_value_with_level(0));
    assert_eq!(0, skipper.max_value_with_level(0));
    assert_eq!(num_docs, skipper.doc_count_with_level(0));
    skipper.advance(skipper.max_doc_id_with_level(0) + 1)?;
    assert_eq!(NO_MORE_DOCS, skipper.min_doc_id_with_level(0));

    reader.close()?;
    writer.close(random)?;
    directory.close()
  }

  // Break on a different value.
  fn test_skipper_few_values_sorted<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let mut config = IndexWriterConfig::new()?;
    config.set_codec(self.get_codec()?);
    let reverse = random.random_bool(0.5);
    config.set_index_sort(Sort::with_fields(vec![SortField::with_reverse(
      Some("dv"),
      SortFieldType::Long,
      reverse,
    )?])?)?;
    let directory = new_directory_shared(random)?;
    let writer = RandomIndexWriter::with_config(random, directory.clone(), config);
    let intervals = random.random_range(2..10);
    let mut num_docs = vec![0; intervals];
    for (value, count) in num_docs.iter_mut().enumerate() {
      *count = random.random_range(0..10) + 16;
      for _ in 0..*count {
        let mut document = Document::new();
        document.add(NumericDocValuesField::indexed_field("dv", value as i64));
        writer.add_document(random, document)?;
      }
    }
    writer.force_merge(random, 1)?;

    let reader = writer.get_reader(random)?;
    let leaf = get_only_leaf_reader(&reader)?;
    let mut skipper = leaf.get_doc_values_skipper("dv")?.unwrap();
    assert_eq!(num_docs.iter().sum::<i32>(), skipper.doc_count());
    skipper.advance(0)?;
    if reverse {
      for value in (0..intervals).rev() {
        assert_eq!(value as i64, skipper.min_value_with_level(0));
        assert_eq!(value as i64, skipper.max_value_with_level(0));
        assert_eq!(num_docs[value], skipper.doc_count_with_level(0));
        skipper.advance(skipper.max_doc_id_with_level(0) + 1)?;
      }
    } else {
      for (value, count) in num_docs.iter().enumerate() {
        assert_eq!(value as i64, skipper.min_value_with_level(0));
        assert_eq!(value as i64, skipper.max_value_with_level(0));
        assert_eq!(*count, skipper.doc_count_with_level(0));
        skipper.advance(skipper.max_doc_id_with_level(0) + 1)?;
      }
    }
    assert_eq!(NO_MORE_DOCS, skipper.min_doc_id_with_level(0));

    reader.close()?;
    writer.close(random)?;
    directory.close()
  }

  // Break on empty doc values.
  fn test_skipper_all_equal_value_with_gaps<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let mut config = IndexWriterConfig::new()?;
    config.set_codec(self.get_codec()?);
    config.set_index_sort(Sort::with_fields(vec![SortField::with_reverse(
      Some("sort"),
      SortFieldType::Long,
      false,
    )?])?)?;
    let directory = new_directory_shared(random)?;
    let writer = RandomIndexWriter::with_config(random, directory.clone(), config);
    let gaps = random.random_range(2..10);
    let mut num_docs = vec![0; gaps];
    let mut total_docs = 0;
    for count in &mut num_docs {
      *count = random.random_range(0..10) + 16;
      for _ in 0..*count {
        let mut document = Document::new();
        document.add(NumericDocValuesField::new("sort", total_docs));
        total_docs += 1;
        document.add(SortedNumericDocValuesField::indexed_field("dv", 0));
        writer.add_document(random, document)?;
      }
      let mut document = Document::new();
      document.add(NumericDocValuesField::new("sort", total_docs));
      total_docs += 1;
      writer.add_document(random, document)?;
    }
    writer.force_merge(random, 1)?;

    let reader = writer.get_reader(random)?;
    let leaf = get_only_leaf_reader(&reader)?;
    let mut skipper = leaf.get_doc_values_skipper("dv")?.unwrap();
    assert_eq!(num_docs.iter().sum::<i32>(), skipper.doc_count());
    skipper.advance(0)?;
    for count in num_docs {
      assert_eq!(0, skipper.min_value_with_level(0));
      assert_eq!(0, skipper.max_value_with_level(0));
      assert_eq!(count, skipper.doc_count_with_level(0));
      skipper.advance(skipper.max_doc_id_with_level(0) + 1)?;
    }
    assert_eq!(NO_MORE_DOCS, skipper.min_doc_id_with_level(0));

    reader.close()?;
    writer.close(random)?;
    directory.close()
  }

  // Break on multi-values.
  fn test_skipper_all_equal_value_with_multi_values<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let mut config = IndexWriterConfig::new()?;
    config.set_codec(self.get_codec()?);
    config.set_index_sort(Sort::with_fields(vec![SortField::with_reverse(
      Some("sort"),
      SortFieldType::Long,
      false,
    )?])?)?;
    let directory = new_directory_shared(random)?;
    let writer = RandomIndexWriter::with_config(random, directory.clone(), config);
    let gaps = random.random_range(2..10);
    let mut num_docs = vec![0; gaps];
    let mut total_docs = 0;
    for i in 0..gaps {
      let docs = random.random_range(0..10) + 16;
      num_docs[i] += docs;
      for _ in 0..docs {
        let mut document = Document::new();
        document.add(NumericDocValuesField::new("sort", total_docs));
        total_docs += 1;
        document.add(SortedNumericDocValuesField::indexed_field("dv", 0));
        writer.add_document(random, document)?;
      }
      if i != gaps - 1 {
        let mut document = Document::new();
        document.add(NumericDocValuesField::new("sort", total_docs));
        total_docs += 1;
        document.add(SortedNumericDocValuesField::indexed_field("dv", 0));
        document.add(SortedNumericDocValuesField::indexed_field("dv", 0));
        writer.add_document(random, document)?;
        num_docs[i + 1] = 1;
      }
    }
    writer.force_merge(random, 1)?;

    let reader = writer.get_reader(random)?;
    let leaf = get_only_leaf_reader(&reader)?;
    let mut skipper = leaf.get_doc_values_skipper("dv")?.unwrap();
    assert_eq!(num_docs.iter().sum::<i32>(), skipper.doc_count());
    skipper.advance(0)?;
    for count in num_docs {
      assert_eq!(0, skipper.min_value_with_level(0));
      assert_eq!(0, skipper.max_value_with_level(0));
      assert_eq!(count, skipper.doc_count_with_level(0));
      skipper.advance(skipper.max_doc_id_with_level(0) + 1)?;
    }
    assert_eq!(NO_MORE_DOCS, skipper.min_doc_id_with_level(0));

    reader.close()?;
    writer.close(random)?;
    directory.close()
  }
}

fn run_case<F>(f: F) -> Result<()>
where
  F: FnOnce(&TestLucene90DocValuesFormatVariableSkipInterval, &mut StdRng) -> Result<()>,
{
  let mut random = random();
  let case = TestLucene90DocValuesFormatVariableSkipInterval {
    skip_index_interval_size: random.random_range(4..16),
  };
  f(&case, &mut random)
}

mod lucene90_doc_values_format_variable_skip_interval_tests {
  use super::run_case;
  use crate::core::util::error::lucene_error::Result;

  #[test]
  fn test_skip_index_interval_size() -> Result<()> {
    run_case(|case, random| case.test_skip_index_interval_size(random))
  }

  #[test]
  fn test_skipper_all_equal_value() -> Result<()> {
    run_case(|case, random| case.test_skipper_all_equal_value(random))
  }

  #[test]
  fn test_skipper_few_values_sorted() -> Result<()> {
    run_case(|case, random| case.test_skipper_few_values_sorted(random))
  }

  #[test]
  fn test_skipper_all_equal_value_with_gaps() -> Result<()> {
    run_case(|case, random| case.test_skipper_all_equal_value_with_gaps(random))
  }

  #[test]
  fn test_skipper_all_equal_value_with_multi_values() -> Result<()> {
    run_case(|case, random| case.test_skipper_all_equal_value_with_multi_values(random))
  }
}

mod base_doc_values_format_test_case_tests {
  use super::run_case;
  use crate::core::util::error::lucene_error::Result;
  use crate::test_framework::core::index::base_doc_values_format_test_case::BaseDocValuesFormatTestCase;

  #[test]
  fn test_sorted_merge_away_all_values_with_skipper() -> Result<()> {
    run_case(|case, random| case.test_sorted_merge_away_all_values_with_skipper(random))
  }

  #[test]
  fn test_sorted_set_merge_away_all_values_with_skipper() -> Result<()> {
    run_case(|case, random| case.test_sorted_set_merge_away_all_values_with_skipper(random))
  }

  #[test]
  fn test_number_merge_away_all_values_with_skipper() -> Result<()> {
    run_case(|case, random| case.test_number_merge_away_all_values_with_skipper(random))
  }

  #[test]
  fn test_sorted_number_merge_away_all_values_with_skipper() -> Result<()> {
    run_case(|case, random| case.test_sorted_number_merge_away_all_values_with_skipper(random))
  }

  #[test]
  fn test_sorted_merge_away_all_values_large_segment_with_skipper() -> Result<()> {
    run_case(|case, random| {
      case.test_sorted_merge_away_all_values_large_segment_with_skipper(random)
    })
  }

  #[test]
  fn test_sorted_set_merge_away_all_values_large_segment_with_skipper() -> Result<()> {
    run_case(|case, random| {
      case.test_sorted_set_merge_away_all_values_large_segment_with_skipper(random)
    })
  }

  #[test]
  fn test_numeric_merge_away_all_values_large_segment_with_skipper() -> Result<()> {
    run_case(|case, random| {
      case.test_numeric_merge_away_all_values_large_segment_with_skipper(random)
    })
  }

  #[test]
  fn test_sorted_numeric_merge_away_all_values_large_segment_with_skipper() -> Result<()> {
    run_case(|case, random| {
      case.test_sorted_numeric_merge_away_all_values_large_segment_with_skipper(random)
    })
  }

  #[test]
  fn test_numeric_doc_values_with_skipper_small() -> Result<()> {
    run_case(|case, random| case.test_numeric_doc_values_with_skipper_small(random))
  }

  #[test]
  fn test_numeric_doc_values_with_skipper_medium() -> Result<()> {
    run_case(|case, random| case.test_numeric_doc_values_with_skipper_medium(random))
  }

  #[cfg(feature = "nightly")]
  #[test]
  #[ignore = "nightly"]
  fn test_numeric_doc_values_with_skipper_big() -> Result<()> {
    run_case(|case, random| case.test_numeric_doc_values_with_skipper_big(random))
  }

  #[test]
  fn test_sorted_numeric_doc_values_with_skipper_small() -> Result<()> {
    run_case(|case, random| case.test_sorted_numeric_doc_values_with_skipper_small(random))
  }

  #[test]
  fn test_sorted_numeric_doc_values_with_skipper_medium() -> Result<()> {
    run_case(|case, random| case.test_sorted_numeric_doc_values_with_skipper_medium(random))
  }
  #[cfg(feature = "nightly")]
  #[test]
  #[ignore = "nightly"]
  fn test_sorted_numeric_doc_values_with_skipper_big() -> Result<()> {
    run_case(|case, random| case.test_sorted_numeric_doc_values_with_skipper_big(random))
  }
  #[test]
  fn test_sorted_doc_values_with_skipper_small() -> Result<()> {
    run_case(|case, random| case.test_sorted_doc_values_with_skipper_small(random))
  }

  #[test]
  fn test_sorted_doc_values_with_skipper_medium() -> Result<()> {
    run_case(|case, random| case.test_sorted_doc_values_with_skipper_medium(random))
  }

  #[cfg(feature = "nightly")]
  #[test]
  #[ignore = "nightly"]
  fn test_sorted_doc_values_with_skipper_big() -> Result<()> {
    run_case(|case, random| case.test_sorted_doc_values_with_skipper_big(random))
  }

  #[test]
  fn test_sorted_set_doc_values_with_skipper_small() -> Result<()> {
    run_case(|case, random| case.test_sorted_set_doc_values_with_skipper_small(random))
  }

  #[test]
  fn test_sorted_set_doc_values_with_skipper_medium() -> Result<()> {
    run_case(|case, random| case.test_sorted_set_doc_values_with_skipper_medium(random))
  }

  #[cfg(feature = "nightly")]
  #[test]
  #[ignore = "nightly"]
  fn test_sorted_set_doc_values_with_skipper_big() -> Result<()> {
    run_case(|case, random| case.test_sorted_set_doc_values_with_skipper_big(random))
  }
  #[test]
  fn test_mismatched_fields() -> Result<()> {
    run_case(|case, random| case.test_mismatched_fields(random))
  }
}

mod legacy_base_doc_values_format_test_case_tests {
  use super::run_case;
  use crate::core::util::error::lucene_error::Result;
  use crate::test_framework::core::index::legacy_base_doc_values_format_test_case::LegacyBaseDocValuesFormatTestCase;

  #[test]
  fn test_one_number() -> Result<()> {
    run_case(|case, random| case.test_one_number(random))
  }

  #[test]
  fn test_one_float() -> Result<()> {
    run_case(|case, random| case.test_one_float(random))
  }

  #[test]
  fn test_two_numbers() -> Result<()> {
    run_case(|case, random| case.test_two_numbers(random))
  }

  #[test]
  fn test_two_binary_values() -> Result<()> {
    run_case(|case, random| case.test_two_binary_values(random))
  }

  #[test]
  fn test_variously_compressible_binary_values() -> Result<()> {
    run_case(|case, random| case.test_variously_compressible_binary_values(random))
  }

  #[test]
  fn test_two_fields_mixed() -> Result<()> {
    run_case(|case, random| case.test_two_fields_mixed(random))
  }

  #[test]
  fn test_three_fields_mixed() -> Result<()> {
    run_case(|case, random| case.test_three_fields_mixed(random))
  }

  #[test]
  fn test_three_fields_mixed2() -> Result<()> {
    run_case(|case, random| case.test_three_fields_mixed2(random))
  }

  #[test]
  fn test_two_documents_numeric() -> Result<()> {
    run_case(|case, random| case.test_two_documents_numeric(random))
  }

  #[test]
  fn test_two_documents_merged() -> Result<()> {
    run_case(|case, random| case.test_two_documents_merged(random))
  }

  #[test]
  fn test_big_numeric_range() -> Result<()> {
    run_case(|case, random| case.test_big_numeric_range(random))
  }

  #[test]
  fn test_big_numeric_range2() -> Result<()> {
    run_case(|case, random| case.test_big_numeric_range2(random))
  }

  #[test]
  fn test_bytes() -> Result<()> {
    run_case(|case, random| case.test_bytes(random))
  }

  #[test]
  fn test_bytes_two_documents_merged() -> Result<()> {
    run_case(|case, random| case.test_bytes_two_documents_merged(random))
  }

  #[test]
  fn test_bytes_merge_away_all_values() -> Result<()> {
    run_case(|case, random| case.test_bytes_merge_away_all_values(random))
  }

  #[test]
  fn test_sorted_bytes() -> Result<()> {
    run_case(|case, random| case.test_sorted_bytes(random))
  }

  #[test]
  fn test_sorted_bytes_two_documents() -> Result<()> {
    run_case(|case, random| case.test_sorted_bytes_two_documents(random))
  }

  #[test]
  fn test_sorted_bytes_three_documents() -> Result<()> {
    run_case(|case, random| case.test_sorted_bytes_three_documents(random))
  }

  #[test]
  fn test_sorted_bytes_two_documents_merged() -> Result<()> {
    run_case(|case, random| case.test_sorted_bytes_two_documents_merged(random))
  }

  #[test]
  fn test_sorted_merge_away_all_values() -> Result<()> {
    run_case(|case, random| case.test_sorted_merge_away_all_values(random))
  }

  #[test]
  fn test_bytes_with_newline() -> Result<()> {
    run_case(|case, random| case.test_bytes_with_newline(random))
  }

  #[test]
  fn test_missing_sorted_bytes() -> Result<()> {
    run_case(|case, random| case.test_missing_sorted_bytes(random))
  }

  #[test]
  fn test_sorted_terms_enum() -> Result<()> {
    run_case(|case, random| case.test_sorted_terms_enum(random))
  }

  #[test]
  fn test_empty_sorted_bytes() -> Result<()> {
    run_case(|case, random| case.test_empty_sorted_bytes(random))
  }

  #[test]
  fn test_empty_bytes() -> Result<()> {
    run_case(|case, random| case.test_empty_bytes(random))
  }

  #[test]
  fn test_very_large_but_legal_bytes() -> Result<()> {
    run_case(|case, random| case.test_very_large_but_legal_bytes(random))
  }

  #[test]
  fn test_very_large_but_legal_sorted_bytes() -> Result<()> {
    run_case(|case, random| case.test_very_large_but_legal_sorted_bytes(random))
  }

  #[test]
  fn test_codec_uses_own_bytes() -> Result<()> {
    run_case(|case, random| case.test_codec_uses_own_bytes(random))
  }

  #[test]
  fn test_codec_uses_own_sorted_bytes() -> Result<()> {
    run_case(|case, random| case.test_codec_uses_own_sorted_bytes(random))
  }

  #[test]
  fn test_doc_values_simple() -> Result<()> {
    run_case(|case, random| case.test_doc_values_simple(random))
  }
  #[test]
  fn test_random_sorted_bytes() -> Result<()> {
    run_case(|case, random| case.test_random_sorted_bytes(random))
  }
  #[test]
  fn test_boolean_numerics_vs_stored_fields() -> Result<()> {
    run_case(|case, random| case.test_boolean_numerics_vs_stored_fields(random))
  }

  #[test]
  fn test_sparse_boolean_numerics_vs_stored_fields() -> Result<()> {
    run_case(|case, random| case.test_sparse_boolean_numerics_vs_stored_fields(random))
  }

  #[test]
  fn test_byte_numerics_vs_stored_fields() -> Result<()> {
    run_case(|case, random| case.test_byte_numerics_vs_stored_fields(random))
  }

  #[test]
  fn test_sparse_byte_numerics_vs_stored_fields() -> Result<()> {
    run_case(|case, random| case.test_sparse_byte_numerics_vs_stored_fields(random))
  }

  #[test]
  fn test_short_numerics_vs_stored_fields() -> Result<()> {
    run_case(|case, random| case.test_short_numerics_vs_stored_fields(random))
  }

  #[test]
  fn test_sparse_short_numerics_vs_stored_fields() -> Result<()> {
    run_case(|case, random| case.test_sparse_short_numerics_vs_stored_fields(random))
  }

  #[test]
  fn test_int_numerics_vs_stored_fields() -> Result<()> {
    run_case(|case, random| case.test_int_numerics_vs_stored_fields(random))
  }

  #[test]
  fn test_sparse_int_numerics_vs_stored_fields() -> Result<()> {
    run_case(|case, random| case.test_sparse_int_numerics_vs_stored_fields(random))
  }

  #[test]
  fn test_long_numerics_vs_stored_fields() -> Result<()> {
    run_case(|case, random| case.test_long_numerics_vs_stored_fields(random))
  }

  #[test]
  fn test_sparse_long_numerics_vs_stored_fields() -> Result<()> {
    run_case(|case, random| case.test_sparse_long_numerics_vs_stored_fields(random))
  }

  #[test]
  fn test_binary_fixed_length_vs_stored_fields() -> Result<()> {
    run_case(|case, random| case.test_binary_fixed_length_vs_stored_fields(random))
  }

  #[test]
  fn test_sparse_binary_fixed_length_vs_stored_fields() -> Result<()> {
    run_case(|case, random| case.test_sparse_binary_fixed_length_vs_stored_fields(random))
  }

  #[test]
  fn test_binary_variable_length_vs_stored_fields() -> Result<()> {
    run_case(|case, random| case.test_binary_variable_length_vs_stored_fields(random))
  }

  #[test]
  fn test_sparse_binary_variable_length_vs_stored_fields() -> Result<()> {
    run_case(|case, random| case.test_sparse_binary_variable_length_vs_stored_fields(random))
  }

  #[test]
  fn test_sorted_fixed_length_vs_stored_fields() -> Result<()> {
    run_case(|case, random| case.test_sorted_fixed_length_vs_stored_fields(random))
  }

  #[test]
  fn test_sparse_sorted_fixed_length_vs_stored_fields() -> Result<()> {
    run_case(|case, random| case.test_sparse_sorted_fixed_length_vs_stored_fields(random))
  }

  #[test]
  fn test_sorted_variable_length_vs_stored_fields() -> Result<()> {
    run_case(|case, random| case.test_sorted_variable_length_vs_stored_fields(random))
  }

  #[test]
  fn test_sparse_sorted_variable_length_vs_stored_fields() -> Result<()> {
    run_case(|case, random| case.test_sparse_sorted_variable_length_vs_stored_fields(random))
  }

  #[test]
  fn test_sorted_set_one_value() -> Result<()> {
    run_case(|case, random| case.test_sorted_set_one_value(random))
  }

  #[test]
  fn test_sorted_set_two_fields() -> Result<()> {
    run_case(|case, random| case.test_sorted_set_two_fields(random))
  }

  #[test]
  fn test_sorted_set_two_documents_merged() -> Result<()> {
    run_case(|case, random| case.test_sorted_set_two_documents_merged(random))
  }

  #[test]
  fn test_sorted_set_two_values() -> Result<()> {
    run_case(|case, random| case.test_sorted_set_two_values(random))
  }

  #[test]
  fn test_sorted_set_two_values_unordered() -> Result<()> {
    run_case(|case, random| case.test_sorted_set_two_values_unordered(random))
  }

  #[test]
  fn test_sorted_set_three_values_two_docs() -> Result<()> {
    run_case(|case, random| case.test_sorted_set_three_values_two_docs(random))
  }

  #[test]
  fn test_sorted_set_two_documents_last_missing() -> Result<()> {
    run_case(|case, random| case.test_sorted_set_two_documents_last_missing(random))
  }

  #[test]
  fn test_sorted_set_two_documents_last_missing_merge() -> Result<()> {
    run_case(|case, random| case.test_sorted_set_two_documents_last_missing_merge(random))
  }

  #[test]
  fn test_sorted_set_two_documents_first_missing() -> Result<()> {
    run_case(|case, random| case.test_sorted_set_two_documents_first_missing(random))
  }

  #[test]
  fn test_sorted_set_two_documents_first_missing_merge() -> Result<()> {
    run_case(|case, random| case.test_sorted_set_two_documents_first_missing_merge(random))
  }

  #[test]
  fn test_sorted_set_merge_away_all_values() -> Result<()> {
    run_case(|case, random| case.test_sorted_set_merge_away_all_values(random))
  }

  #[test]
  fn test_sorted_set_terms_enum() -> Result<()> {
    run_case(|case, random| case.test_sorted_set_terms_enum(random))
  }

  #[test]
  fn test_sorted_set_fixed_length_vs_stored_fields() -> Result<()> {
    run_case(|case, random| case.test_sorted_set_fixed_length_vs_stored_fields(random))
  }

  #[test]
  fn test_sorted_numerics_single_valued_vs_stored_fields() -> Result<()> {
    run_case(|case, random| case.test_sorted_numerics_single_valued_vs_stored_fields(random))
  }

  #[test]
  fn test_sorted_numerics_single_valued_missing_vs_stored_fields() -> Result<()> {
    run_case(|case, random| {
      case.test_sorted_numerics_single_valued_missing_vs_stored_fields(random)
    })
  }

  #[test]
  fn test_sorted_numerics_multiple_values_vs_stored_fields() -> Result<()> {
    run_case(|case, random| case.test_sorted_numerics_multiple_values_vs_stored_fields(random))
  }

  #[test]
  fn test_sorted_numerics_few_unique_sets_vs_stored_fields() -> Result<()> {
    run_case(|case, random| case.test_sorted_numerics_few_unique_sets_vs_stored_fields(random))
  }

  #[test]
  fn test_sorted_set_variable_length_vs_stored_fields() -> Result<()> {
    run_case(|case, random| case.test_sorted_set_variable_length_vs_stored_fields(random))
  }

  #[test]
  fn test_sorted_set_fixed_length_single_valued_vs_stored_fields() -> Result<()> {
    run_case(|case, random| {
      case.test_sorted_set_fixed_length_single_valued_vs_stored_fields(random)
    })
  }

  #[test]
  fn test_sorted_set_variable_length_single_valued_vs_stored_fields() -> Result<()> {
    run_case(|case, random| {
      case.test_sorted_set_variable_length_single_valued_vs_stored_fields(random)
    })
  }

  #[test]
  fn test_sorted_set_fixed_length_few_unique_sets_vs_stored_fields() -> Result<()> {
    run_case(|case, random| {
      case.test_sorted_set_fixed_length_few_unique_sets_vs_stored_fields(random)
    })
  }

  #[test]
  fn test_sorted_set_variable_length_few_unique_sets_vs_stored_fields() -> Result<()> {
    run_case(|case, random| {
      case.test_sorted_set_variable_length_few_unique_sets_vs_stored_fields(random)
    })
  }

  #[test]
  fn test_sorted_set_variable_length_many_values_per_doc_vs_stored_fields() -> Result<()> {
    run_case(|case, random| {
      case.test_sorted_set_variable_length_many_values_per_doc_vs_stored_fields(random)
    })
  }

  #[test]
  fn test_sorted_set_fixed_length_many_values_per_doc_vs_stored_fields() -> Result<()> {
    run_case(|case, random| {
      case.test_sorted_set_fixed_length_many_values_per_doc_vs_stored_fields(random)
    })
  }

  #[test]
  fn test_gcd_compression() -> Result<()> {
    run_case(|case, random| case.test_gcd_compression(random))
  }

  #[test]
  fn test_sparse_gcd_compression() -> Result<()> {
    run_case(|case, random| case.test_sparse_gcd_compression(random))
  }

  #[test]
  fn test_zeros() -> Result<()> {
    run_case(|case, random| case.test_zeros(random))
  }

  #[test]
  fn test_sparse_zeros() -> Result<()> {
    run_case(|case, random| case.test_sparse_zeros(random))
  }

  #[test]
  fn test_zero_or_min() -> Result<()> {
    run_case(|case, random| case.test_zero_or_min(random))
  }

  #[test]
  fn test_two_numbers_one_missing() -> Result<()> {
    run_case(|case, random| case.test_two_numbers_one_missing(random))
  }

  #[test]
  fn test_two_numbers_one_missing_with_merging() -> Result<()> {
    run_case(|case, random| case.test_two_numbers_one_missing_with_merging(random))
  }

  #[test]
  fn test_three_numbers_one_missing_with_merging() -> Result<()> {
    run_case(|case, random| case.test_three_numbers_one_missing_with_merging(random))
  }

  #[test]
  fn test_two_bytes_one_missing() -> Result<()> {
    run_case(|case, random| case.test_two_bytes_one_missing(random))
  }

  #[test]
  fn test_two_bytes_one_missing_with_merging() -> Result<()> {
    run_case(|case, random| case.test_two_bytes_one_missing_with_merging(random))
  }

  #[test]
  fn test_three_bytes_one_missing_with_merging() -> Result<()> {
    run_case(|case, random| case.test_three_bytes_one_missing_with_merging(random))
  }
  #[test]
  fn test_threads() -> Result<()> {
    run_case(|case, random| case.test_threads(random))
  }
  #[cfg(feature = "nightly")]
  #[test]
  #[ignore = "nightly"]
  fn test_threads2() -> Result<()> {
    run_case(|case, random| case.test_threads2(random))
  }
  #[cfg(feature = "nightly")]
  #[test]
  #[ignore = "nightly"]
  fn test_threads3() -> Result<()> {
    run_case(|case, random| case.test_threads3(random))
  }
  #[test]
  fn test_empty_binary_value_on_page_sizes() -> Result<()> {
    run_case(|case, random| case.test_empty_binary_value_on_page_sizes(random))
  }

  #[test]
  fn test_one_sorted_number() -> Result<()> {
    run_case(|case, random| case.test_one_sorted_number(random))
  }

  #[test]
  fn test_one_sorted_number_one_missing() -> Result<()> {
    run_case(|case, random| case.test_one_sorted_number_one_missing(random))
  }

  #[test]
  fn test_number_merge_away_all_values() -> Result<()> {
    run_case(|case, random| case.test_number_merge_away_all_values(random))
  }

  #[test]
  fn test_two_sorted_number() -> Result<()> {
    run_case(|case, random| case.test_two_sorted_number(random))
  }

  #[test]
  fn test_two_sorted_number_same_value() -> Result<()> {
    run_case(|case, random| case.test_two_sorted_number_same_value(random))
  }

  #[test]
  fn test_two_sorted_number_one_missing() -> Result<()> {
    run_case(|case, random| case.test_two_sorted_number_one_missing(random))
  }

  #[test]
  fn test_sorted_number_merge() -> Result<()> {
    run_case(|case, random| case.test_sorted_number_merge(random))
  }

  #[test]
  fn test_sorted_number_merge_away_all_values() -> Result<()> {
    run_case(|case, random| case.test_sorted_number_merge_away_all_values(random))
  }

  #[test]
  fn test_sorted_enum_advance_independently() -> Result<()> {
    run_case(|case, random| case.test_sorted_enum_advance_independently(random))
  }

  #[test]
  fn test_sorted_set_enum_advance_independently() -> Result<()> {
    run_case(|case, random| case.test_sorted_set_enum_advance_independently(random))
  }

  #[test]
  fn test_sorted_merge_away_all_values_large_segment() -> Result<()> {
    run_case(|case, random| case.test_sorted_merge_away_all_values_large_segment(random))
  }

  #[test]
  fn test_sorted_set_merge_away_all_values_large_segment() -> Result<()> {
    run_case(|case, random| case.test_sorted_set_merge_away_all_values_large_segment(random))
  }

  #[test]
  fn test_numeric_merge_away_all_values_large_segment() -> Result<()> {
    run_case(|case, random| case.test_numeric_merge_away_all_values_large_segment(random))
  }

  #[test]
  fn test_sorted_numeric_merge_away_all_values_large_segment() -> Result<()> {
    run_case(|case, random| case.test_sorted_numeric_merge_away_all_values_large_segment(random))
  }

  #[test]
  fn test_binary_merge_away_all_values_large_segment() -> Result<()> {
    run_case(|case, random| case.test_binary_merge_away_all_values_large_segment(random))
  }

  #[test]
  fn test_random_advance_numeric() -> Result<()> {
    run_case(|case, random| case.test_random_advance_numeric(random))
  }

  #[test]
  fn test_random_advance_binary() -> Result<()> {
    run_case(|case, random| case.test_random_advance_binary(random))
  }

  #[cfg(feature = "nightly")]
  #[test]
  #[ignore = "nightly"]
  fn test_high_ords_sorted_set_dv() -> Result<()> {
    run_case(|case, random| case.test_high_ords_sorted_set_dv(random))
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
