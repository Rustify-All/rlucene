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
use crate::core::codecs::lucene99::lucene99_hnsw_scalar_quantized_vectors_format::Lucene99HnswScalarQuantizedVectorsFormat;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::index::base_index_file_format_test_case::BaseIndexFileFormatTestCase;
use crate::test_framework::core::index::base_knn_vectors_format_test_case::{
  BaseKnnVectorsFormatTestCase, BaseKnnVectorsFormatTestCaseState,
};
use crate::test_framework::core::util::lucene_test_case::random;
use crate::test_framework::core::util::test_util::TestUtil;
use rand::prelude::StdRng;

#[allow(dead_code)] // for quick search
pub struct TestLucene99HnswScalarQuantizedVectorsFormat {
  base_knn_vectors_format_test_case_state: BaseKnnVectorsFormatTestCaseState,
}

impl BaseIndexFileFormatTestCase for TestLucene99HnswScalarQuantizedVectorsFormat {
  type Defaults = crate::test_framework::core::index::base_knn_vectors_format_test_case::BaseKnnVectorsFormatTestCaseDefaults;

  fn get_codec(&self) -> Result<Codecs> {
    Ok(TestUtil::always_knn_vectors_format(Lucene99HnswScalarQuantizedVectorsFormat::new()?).into())
  }
}

impl BaseKnnVectorsFormatTestCase for TestLucene99HnswScalarQuantizedVectorsFormat {
  fn base_knn_vectors_format_test_case_state(&self) -> &BaseKnnVectorsFormatTestCaseState {
    &self.base_knn_vectors_format_test_case_state
  }
}

fn run_case<F>(f: F) -> Result<()>
where
  F: FnOnce(&TestLucene99HnswScalarQuantizedVectorsFormat, &mut StdRng) -> Result<()>,
{
  let mut random = random();
  let case = TestLucene99HnswScalarQuantizedVectorsFormat {
    base_knn_vectors_format_test_case_state: BaseKnnVectorsFormatTestCaseState::new(&mut random),
  };
  f(&case, &mut random)
}

mod base_knn_vectors_format_test_case_test {
  use super::run_case;
  use crate::core::util::error::lucene_error::Result;
  use crate::test_framework::core::index::base_knn_vectors_format_test_case::BaseKnnVectorsFormatTestCase;

  #[test]
  fn test_field_constructor() -> Result<()> {
    run_case(|case, random| case.test_field_constructor(random))
  }

  #[test]
  fn test_field_constructor_exceptions() -> Result<()> {
    run_case(|case, random| case.test_field_constructor_exceptions(random))
  }

  #[test]
  fn test_field_set_value() -> Result<()> {
    run_case(|case, random| case.test_field_set_value(random))
  }

  #[test]
  fn test_illegal_dim_change_two_docs() -> Result<()> {
    run_case(|case, random| case.test_illegal_dim_change_two_docs(random))
  }

  #[test]
  fn test_illegal_similarity_function_change() -> Result<()> {
    run_case(|case, random| case.test_illegal_similarity_function_change(random))
  }

  #[test]
  fn test_illegal_dim_change_two_writers() -> Result<()> {
    run_case(|case, random| case.test_illegal_dim_change_two_writers(random))
  }

  #[test]
  fn test_merging_with_different_knn_fields() -> Result<()> {
    run_case(|case, random| case.test_merging_with_different_knn_fields(random))
  }

  #[test]
  fn test_merging_with_different_byte_knn_fields() -> Result<()> {
    run_case(|case, random| case.test_merging_with_different_byte_knn_fields(random))
  }

  #[test]
  fn test_writer_ram_estimate() -> Result<()> {
    run_case(|case, random| case.test_writer_ram_estimate(random))
  }

  #[test]
  fn test_illegal_similarity_function_change_two_writers() -> Result<()> {
    run_case(|case, random| case.test_illegal_similarity_function_change_two_writers(random))
  }

  #[test]
  fn test_add_indexes_directory0() -> Result<()> {
    run_case(|case, random| case.test_add_indexes_directory0(random))
  }

  #[test]
  fn test_add_indexes_directory1() -> Result<()> {
    run_case(|case, random| case.test_add_indexes_directory1(random))
  }

  #[test]
  fn test_add_indexes_directory01() -> Result<()> {
    run_case(|case, random| case.test_add_indexes_directory01(random))
  }

  #[test]
  fn test_illegal_dim_change_via_add_indexes_directory() -> Result<()> {
    run_case(|case, random| case.test_illegal_dim_change_via_add_indexes_directory(random))
  }

  #[test]
  fn test_illegal_similarity_function_change_via_add_indexes_directory() -> Result<()> {
    run_case(|case, random| {
      case.test_illegal_similarity_function_change_via_add_indexes_directory(random)
    })
  }

  #[test]
  fn test_illegal_dim_change_via_add_indexes_codec_reader() -> Result<()> {
    run_case(|case, random| case.test_illegal_dim_change_via_add_indexes_codec_reader(random))
  }

  #[test]
  fn test_illegal_similarity_function_change_via_add_indexes_codec_reader() -> Result<()> {
    run_case(|case, random| {
      case.test_illegal_similarity_function_change_via_add_indexes_codec_reader(random)
    })
  }

  #[test]
  fn test_illegal_dim_change_via_add_indexes_slow_codec_reader() -> Result<()> {
    run_case(|case, random| case.test_illegal_dim_change_via_add_indexes_slow_codec_reader(random))
  }

  #[test]
  fn test_illegal_similarity_function_change_via_add_indexes_slow_codec_reader() -> Result<()> {
    run_case(|case, random| {
      case.test_illegal_similarity_function_change_via_add_indexes_slow_codec_reader(random)
    })
  }

  #[test]
  fn test_illegal_multiple_values() -> Result<()> {
    run_case(|case, random| case.test_illegal_multiple_values(random))
  }

  #[test]
  fn test_illegal_dimension_too_large() -> Result<()> {
    run_case(|case, random| case.test_illegal_dimension_too_large(random))
  }

  #[test]
  fn test_illegal_empty_vector() -> Result<()> {
    run_case(|case, random| case.test_illegal_empty_vector(random))
  }

  #[test]
  fn test_different_codecs1() -> Result<()> {
    run_case(|case, random| case.test_different_codecs1(random))
  }

  #[test]
  fn test_different_codecs2() -> Result<()> {
    run_case(|case, random| case.test_different_codecs2(random))
  }

  #[test]
  fn test_invalid_knn_vector_field_usage() -> Result<()> {
    run_case(|case, random| case.test_invalid_knn_vector_field_usage(random))
  }

  #[test]
  fn test_delete_all_vector_docs() -> Result<()> {
    run_case(|case, random| case.test_delete_all_vector_docs(random))
  }

  #[test]
  fn test_knn_vector_field_missing_from_one_segment() -> Result<()> {
    run_case(|case, random| case.test_knn_vector_field_missing_from_one_segment(random))
  }

  #[test]
  fn test_sparse_vectors() -> Result<()> {
    run_case(|case, random| case.test_sparse_vectors(random))
  }

  #[test]
  fn test_float_vector_scorer_iteration() -> Result<()> {
    run_case(|case, random| case.test_float_vector_scorer_iteration(random))
  }

  #[test]
  fn test_byte_vector_scorer_iteration() -> Result<()> {
    run_case(|case, random| case.test_byte_vector_scorer_iteration(random))
  }

  #[test]
  fn test_empty_float_vector_data() -> Result<()> {
    run_case(|case, random| case.test_empty_float_vector_data(random))
  }

  #[test]
  fn test_empty_byte_vector_data() -> Result<()> {
    run_case(|case, random| case.test_empty_byte_vector_data(random))
  }

  #[test]
  fn test_indexed_value_not_aliased() -> Result<()> {
    run_case(|case, random| case.test_indexed_value_not_aliased(random))
  }

  #[test]
  fn test_sorted_index() -> Result<()> {
    run_case(|case, random| case.test_sorted_index(random))
  }

  #[test]
  fn test_sorted_index_bytes() -> Result<()> {
    run_case(|case, random| case.test_sorted_index_bytes(random))
  }

  #[test]
  fn test_index_multiple_knn_vector_fields() -> Result<()> {
    run_case(|case, random| case.test_index_multiple_knn_vector_fields(random))
  }

  #[test]
  fn test_random() -> Result<()> {
    run_case(|case, random| case.test_random(random))
  }

  #[test]
  fn test_random_bytes() -> Result<()> {
    run_case(|case, random| case.test_random_bytes(random))
  }

  #[test]
  fn test_search_with_visited_limit() -> Result<()> {
    run_case(|case, random| case.test_search_with_visited_limit(random))
  }

  #[test]
  fn test_random_with_updates_and_graph() -> Result<()> {
    run_case(|case, random| case.test_random_with_updates_and_graph(random))
  }

  #[test]
  fn test_check_index_includes_vectors() -> Result<()> {
    run_case(|case, random| case.test_check_index_includes_vectors(random))
  }

  #[test]
  fn test_similarity_function_identifiers() -> Result<()> {
    run_case(|case, _random| case.test_similarity_function_identifiers())
  }

  #[test]
  fn test_vector_encoding_ordinals() -> Result<()> {
    run_case(|case, _random| case.test_vector_encoding_ordinals())
  }

  #[test]
  fn test_advance() -> Result<()> {
    run_case(|case, random| case.test_advance(random))
  }

  #[test]
  fn test_vector_values_report_correct_docs() -> Result<()> {
    run_case(|case, random| case.test_vector_values_report_correct_docs(random))
  }

  #[test]
  fn test_mismatched_fields() -> Result<()> {
    run_case(|case, random| case.test_mismatched_fields(random))
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
