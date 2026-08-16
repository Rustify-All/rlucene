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
use crate::core::document::fields::Fields;
use crate::core::document::knn_byte_vector_field::KnnByteVectorField;
use crate::core::index::directory_reader;
use crate::core::index::term::Term;
use crate::core::index::vector_similarity_function::VectorSimilarityFunction;
use crate::core::search::knn_byte_vector_query::KnnByteVectorQuery;
use crate::core::search::knn_float_vector_query::KnnFloatVectorQuery;
use crate::core::search::match_all_docs_query::MatchAllDocsQuery;
use crate::core::search::query::Query;
use crate::core::search::query::QueryBase;
use crate::core::search::term_query::TermQuery;
use crate::core::store::directory::{DirEnum, MaybeNrtDirEnum, MockDirWrapper};
use crate::core::util::error::lucene_error::LuceneError;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::search::base_knn_vector_query_test_case::BaseKnnVectorQueryTestCase;
use crate::test_framework::core::store::mock_directory_wrapper::MockDirectoryWrapper;
use crate::test_framework::core::util::lucene_test_case::{
  DirectoryImpl, new_directory_impl, new_searcher_with_reader, random,
};
use crate::test_framework::core::util::test_vector_util::random_vector_bytes_dim;
use rand::rngs::StdRng;
use rand::{Rng, RngExt};
use std::sync::Arc;

#[allow(dead_code)] // for quick search
pub(crate) struct TestKnnByteVectorQuery {
  hook: TestKnnByteVectorQueryHook,
}

enum TestKnnByteVectorQueryHook {
  Default,
  MMap,
}

impl TestKnnByteVectorQuery {
  fn new() -> Self {
    Self {
      hook: TestKnnByteVectorQueryHook::Default,
    }
  }

  pub(crate) fn new_mmap() -> Self {
    Self {
      hook: TestKnnByteVectorQueryHook::MMap,
    }
  }
}

fn run_case<F>(f: F) -> Result<()>
where
  F: FnOnce(&TestKnnByteVectorQuery, &mut StdRng) -> Result<()>,
{
  let case = TestKnnByteVectorQuery::new();
  let mut random = random();
  f(&case, &mut random)
}

mod base_knn_vector_query_test_case_tests {
  use super::run_case;
  use crate::core::util::error::lucene_error::Result;
  use crate::test_framework::core::search::base_knn_vector_query_test_case::BaseKnnVectorQueryTestCase;

  #[test]
  fn test_equals() -> Result<()> {
    run_case(|case, _random| case.test_equals())
  }

  #[test]
  fn test_get_field() -> Result<()> {
    run_case(|case, _random| case.test_get_field())
  }

  #[test]
  fn test_get_k() -> Result<()> {
    run_case(|case, _random| case.test_get_k())
  }

  #[test]
  fn test_get_filter() -> Result<()> {
    run_case(|case, _random| case.test_get_filter())
  }

  #[test]
  fn test_empty_index() -> Result<()> {
    run_case(|case, _random| case.test_empty_index())
  }

  #[test]
  fn test_find_all() -> Result<()> {
    run_case(|case, random| case.test_find_all(random))
  }

  #[test]
  fn test_find_fewer() -> Result<()> {
    run_case(|case, random| case.test_find_fewer(random))
  }

  #[test]
  fn test_search_boost() -> Result<()> {
    run_case(|case, random| case.test_search_boost(random))
  }

  #[test]
  fn test_simple_filter() -> Result<()> {
    run_case(|case, random| case.test_simple_filter(random))
  }

  #[test]
  fn test_filter_with_no_vector_matches() -> Result<()> {
    run_case(|case, random| case.test_filter_with_no_vector_matches(random))
  }

  #[test]
  fn test_dimension_mismatch() -> Result<()> {
    run_case(|case, random| case.test_dimension_mismatch(random))
  }

  #[test]
  fn test_non_vector_field() -> Result<()> {
    run_case(|case, random| case.test_non_vector_field(random))
  }

  #[test]
  fn test_illegal_arguments() -> Result<()> {
    run_case(|case, _random| case.test_illegal_arguments())
  }

  #[test]
  fn test_different_reader() -> Result<()> {
    run_case(|case, random| case.test_different_reader(random))
  }

  #[test]
  fn test_score_euclidean() -> Result<()> {
    run_case(|case, random| case.test_score_euclidean(random))
  }

  #[test]
  fn test_score_cosine() -> Result<()> {
    run_case(|case, random| case.test_score_cosine(random))
  }

  #[test]
  fn test_score_mip() -> Result<()> {
    run_case(|case, random| case.test_score_mip(random))
  }

  #[test]
  fn test_explain() -> Result<()> {
    run_case(|case, random| case.test_explain(random))
  }

  #[test]
  fn test_explain_multiple_segments() -> Result<()> {
    run_case(|case, random| case.test_explain_multiple_segments(random))
  }

  #[test]
  fn test_skewed_index() -> Result<()> {
    run_case(|case, random| case.test_skewed_index(random))
  }

  #[test]
  fn test_random() -> Result<()> {
    run_case(|case, random| case.test_random(random))
  }

  #[test]
  fn test_filter_with_same_score() -> Result<()> {
    run_case(|case, random| case.test_filter_with_same_score(random))
  }

  #[test]
  fn test_random_with_filter() -> Result<()> {
    run_case(|case, random| case.test_random_with_filter(random))
  }

  #[test]
  fn test_deletes() -> Result<()> {
    run_case(|case, random| case.test_deletes(random))
  }

  #[test]
  fn test_all_deletes() -> Result<()> {
    run_case(|case, random| case.test_all_deletes(random))
  }

  #[test]
  fn test_merge_away_all_values() -> Result<()> {
    run_case(|case, random| case.test_merge_away_all_values(random))
  }

  #[test]
  fn test_no_live_docs_reader() -> Result<()> {
    run_case(|case, random| case.test_no_live_docs_reader(random))
  }

  #[test]
  #[ignore = "BitSet filter reuse is not implemented"]
  fn test_bit_set_query() -> Result<()> {
    run_case(|case, random| case.test_bit_set_query(random))
  }

  #[test]
  fn test_time_limiting_knn_collector_manager() -> Result<()> {
    run_case(|case, random| case.test_time_limiting_knn_collector_manager(random))
  }

  #[test]
  fn test_timeout() -> Result<()> {
    run_case(|case, random| case.test_timeout(random))
  }

  #[test]
  fn test_same_field_different_formats() -> Result<()> {
    run_case(|case, random| case.test_same_field_different_formats(random))
  }
}

#[test]
fn test_to_string() -> Result<()> {
  run_case(|case, random| case.test_to_string(random))
}

#[test]
fn test_get_target() -> Result<()> {
  run_case(|case, _random| case.test_get_target())
}

#[test]
fn test_vector_encoding_mismatch() -> Result<()> {
  run_case(|case, random| case.test_vector_encoding_mismatch(random))
}

impl BaseKnnVectorQueryTestCase for TestKnnByteVectorQuery {
  type KnnVectorQuery = KnnByteVectorQuery;

  fn get_knn_vector_query(
    &self,
    field: &str,
    query: Vec<f32>,
    k: usize,
    query_filter: Option<Query>,
  ) -> Result<Self::KnnVectorQuery> {
    KnnByteVectorQuery::with_filter(field, float_to_bytes(query), k, query_filter)
  }

  fn get_throwing_knn_vector_query(
    &self,
    field: &str,
    query: Vec<f32>,
    k: usize,
    query_filter: Option<Query>,
  ) -> Result<Self::KnnVectorQuery> {
    KnnByteVectorQuery::throwing_with_filter(field, float_to_bytes(query), k, query_filter)
  }

  fn get_knn_vector_query_no_filter(
    &self,
    field: &str,
    query: Vec<f32>,
    k: usize,
  ) -> Result<Self::KnnVectorQuery> {
    self.get_knn_vector_query(field, query, k, None)
  }

  fn random_vector<R>(&self, random: &mut R, dim: usize) -> Vec<f32>
  where
    R: Rng + ?Sized,
  {
    let v = random_vector_bytes_dim(random, dim);
    v.into_iter().map(|value| (value as i8) as f32).collect()
  }

  fn get_knn_vector_field_with_similarity(
    &self,
    name: &str,
    vector: Vec<f32>,
    similarity_function: VectorSimilarityFunction,
  ) -> Result<Fields> {
    Ok(
      KnnByteVectorField::with_similarity_function(
        name,
        float_to_bytes(vector),
        similarity_function,
      )?
      .into(),
    )
  }

  fn get_knn_vector_field(&self, name: &str, vector: Vec<f32>) -> Result<Fields> {
    Ok(KnnByteVectorField::new(name, float_to_bytes(vector))?.into())
  }

  type Directory = Arc<DirEnum>;

  fn new_directory_for_test<R>(&self, random: &mut R) -> Result<Self::Directory>
  where
    R: Rng + ?Sized,
  {
    match self.hook {
      TestKnnByteVectorQueryHook::Default => self.default_new_directory_for_test(random),
      TestKnnByteVectorQueryHook::MMap => {
        let directory = new_directory_impl(random, DirectoryImpl::MMapDirectory)?;
        let mock = MockDirectoryWrapper::new(random, MaybeNrtDirEnum::Raw(directory));
        Ok(Arc::new(DirEnum::B(MockDirWrapper::from_inner(mock))))
      },
    }
  }
}

impl TestKnnByteVectorQuery {
  pub(crate) fn test_to_string<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let index_store = self.get_index_store(
      random,
      "field",
      &[vec![0.0, 1.0], vec![1.0, 2.0], vec![0.0, 0.0]],
    )?;
    let reader = directory_reader::open(index_store)?;
    let searcher = new_searcher_with_reader(reader)?;

    let query = self.get_knn_vector_query_no_filter("field", vec![0.0, 1.0], 10)?;
    assert_eq!(
      "KnnByteVectorQuery:field[0,...][10]",
      query.to_string("ignored")?
    );

    let rewritten = searcher.rewrite(query.clone())?;
    self.assert_doc_score_query_to_string(&rewritten)?;

    // test with filter
    let filter: Query = TermQuery::new(Term::from_text("id", "text")).into();
    let query = self.get_knn_vector_query("field", vec![0.0, 1.0], 10, Some(filter))?;
    assert_eq!(
      "KnnByteVectorQuery:field[0,...][10][id:text]",
      query.to_string("ignored")?
    );
    Ok(())
  }

  pub(crate) fn test_get_target(&self) -> Result<()> {
    let query_vector_bytes = float_to_bytes(vec![0.0, 1.0]);
    let query = KnnByteVectorQuery::new("f1", query_vector_bytes.clone(), 10)?;
    let copy = query.get_target_copy();
    assert_eq!(query_vector_bytes, copy);
    assert_ne!(query_vector_bytes.as_ptr(), copy.as_ptr());
    Ok(())
  }

  pub(crate) fn test_vector_encoding_mismatch<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let index_store = self.get_index_store(
      random,
      "field",
      &[vec![0.0, 1.0], vec![1.0, 2.0], vec![0.0, 0.0]],
    )?;
    let reader = directory_reader::open(index_store)?;
    let searcher = new_searcher_with_reader(reader)?;
    let filter = if random.random_bool(0.5) {
      Some(MatchAllDocsQuery::new().into())
    } else {
      None
    };
    let query = KnnFloatVectorQuery::with_filter("field", vec![0.0, 1.0], 10, filter)?;
    match searcher.search(query, 10) {
      Err(LuceneError::IllegalState(_)) => Ok(()),
      _ => unreachable!(""),
    }
  }
}

fn float_to_bytes(query: Vec<f32>) -> Vec<u8> {
  query
    .into_iter()
    .map(|value| {
      assert!(
        value <= i8::MAX as f32 && value >= i8::MIN as f32 && value.fract() == 0.0,
        "float value cannot be converted to byte; provided: {value}"
      );
      (value as i8) as u8
    })
    .collect()
}
