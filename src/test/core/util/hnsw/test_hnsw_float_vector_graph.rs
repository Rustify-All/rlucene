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

use crate::core::codecs::hnsw::default_flat_vector_scorer::{ByteVectorScorer, FloatVectorScorer};
use crate::core::codecs::hnsw::flat_vectors_scorer::FlatVectorsScorer;
use crate::core::codecs::knn_field_vectors_writer::VectorValueEnum;
use crate::core::document::field::Field;
use crate::core::document::knn_float_vector_field::KnnFloatVectorField;
use crate::core::index::byte_vector_values::ByteVectorValues;
use crate::core::index::float_vector_values::FloatVectorValues;
use crate::core::index::knn_vector_values::{KnnVectorValues, KnnVectorValuesEnm2};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::vector_encoding::VectorEncoding;
use crate::core::index::vector_similarity_function::VectorSimilarityFunction;
use crate::core::search::knn_collector::KnnCollector;
use crate::core::search::knn_float_vector_query::KnnFloatVectorQuery;
use crate::core::search::query::Query;
use crate::core::util::clone::TryClone;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::hnsw::hnsw_builder::HnswBuilder;
use crate::core::util::hnsw::hnsw_graph_builder::create;
use crate::core::util::hnsw::hnsw_graph_searcher::search_with_top_k;
use crate::core::util::hnsw::random_vector_scorer::RandomVectorScorerEnum2;
use crate::test_framework::core::util::hnsw::hnsw_graph_test_case::{
  CircularFloatVectorValues, HnswGraphTestCase, TestsCircularKnnVectorValues, TestsKnnVectorValues,
  create_random_float_vectors, random_vector,
};
use crate::test_framework::core::util::hnsw::mock_vector_values::MockVectorValues;
use crate::test_framework::core::util::lucene_test_case::random;
use rand::prelude::StdRng;
use rand::{Rng, RngExt};
use std::sync::Arc;

#[allow(dead_code)] // for quick search
pub struct TestHnswFloatVectorGraph {
  pub(crate) similarity_function: VectorSimilarityFunction,
}
impl TestHnswFloatVectorGraph {
  pub fn new<R>(random: &mut R) -> Self
  where
    R: Rng + ?Sized,
  {
    let similarity_function = VectorSimilarityFunction::random(random);
    TestHnswFloatVectorGraph {
      similarity_function,
    }
  }
}

fn run_case<F>(f: F) -> Result<()>
where
  F: FnOnce(&mut TestHnswFloatVectorGraph, &mut StdRng) -> Result<()>,
{
  let mut random = random();
  let mut case = TestHnswFloatVectorGraph::new(&mut random);
  f(&mut case, &mut random)
}

mod hnsw_graph_test_case_tests {
  use crate::core::util::error::lucene_error::Result;
  use crate::test::core::util::hnsw::test_hnsw_float_vector_graph::run_case;
  use crate::test_framework::core::util::hnsw::hnsw_graph_test_case::HnswGraphTestCase;

  #[test]
  fn test_random_read_write_and_merge() -> Result<()> {
    run_case(|case, random| case.test_random_read_write_and_merge(random))
  }
  #[test]
  fn test_read_write() -> Result<()> {
    run_case(|case, random| case.test_read_write(random))
  }

  #[test]
  fn test_sorted_and_unsorted_indices_return_same_results() -> Result<()> {
    run_case(|case, random| case.test_sorted_and_unsorted_indices_return_same_results(random))
  }

  #[test]
  fn test_aknn_diverse() -> Result<()> {
    run_case(|case, random| case.test_aknn_diverse(random))
  }

  #[test]
  fn test_search_with_accept_ords() -> Result<()> {
    run_case(|case, random| case.test_search_with_accept_ords(random))
  }

  #[test]
  fn test_search_with_selective_accept_ords() -> Result<()> {
    run_case(|case, random| case.test_search_with_selective_accept_ords(random))
  }

  #[test]
  fn test_hnsw_graph_builder_initialization_from_graph_with_offset_zero() -> Result<()> {
    run_case(|case, random| {
      case.test_hnsw_graph_builder_initialization_from_graph_with_offset_zero(random)
    })
  }

  #[test]
  fn test_hnsw_graph_builder_initialization_from_graph_with_non_zero_offset() -> Result<()> {
    run_case(|case, random| {
      case.test_hnsw_graph_builder_initialization_from_graph_with_non_zero_offset(random)
    })
  }

  #[test]
  fn test_visited_limit() -> Result<()> {
    run_case(|case, random| case.test_visited_limit(random))
  }

  #[test]
  fn test_hnsw_graph_builder_invalid() -> Result<()> {
    run_case(|case, random| case.test_hnsw_graph_builder_invalid(random))
  }

  #[test]
  fn test_ram_usage_estimate() -> Result<()> {
    run_case(|case, random| case.test_ram_usage_estimate(random))
  }

  #[test]
  fn test_diversity() -> Result<()> {
    run_case(|case, random| case.test_diversity(random))
  }

  #[test]
  fn test_diversity_fallback() -> Result<()> {
    run_case(|case, random| case.test_diversity_fallback(random))
  }

  #[test]
  fn test_diversity_3d() -> Result<()> {
    run_case(|case, random| case.test_diversity_3d(random))
  }

  #[test]
  fn test_random() -> Result<()> {
    run_case(|case, random| case.test_random(random))
  }

  #[test]
  fn test_on_heap_hnsw_graph_search() -> Result<()> {
    run_case(|case, random| case.test_on_heap_hnsw_graph_search(random))
  }

  #[test]
  fn test_concurrent_merge_builder() -> Result<()> {
    run_case(|case, random| case.test_concurrent_merge_builder(random))
  }

  #[test]
  fn test_all_nodes_visited_in_single_level() -> Result<()> {
    run_case(|case, random| case.test_all_nodes_visited_in_single_level(random))
  }
}

impl HnswGraphTestCase<Vec<f32>> for TestHnswFloatVectorGraph {
  fn score(&self, query: &Vec<f32>, vector: &Vec<f32>) -> f32 {
    self
      .similarity_function()
      .compare_f32(query, vector)
      .unwrap()
  }

  fn set_similarity_function(&mut self, s: VectorSimilarityFunction) {
    self.similarity_function = s
  }

  fn similarity_function(&self) -> VectorSimilarityFunction {
    self.similarity_function
  }

  fn get_vector_encoding(&self) -> VectorEncoding {
    VectorEncoding::FLOAT32(4)
  }

  fn knn_query(&self, field: &str, vector: Vec<f32>, k: usize) -> Result<Query> {
    Ok(KnnFloatVectorQuery::new(field, vector, k)?.into())
  }

  fn random_vector<R>(&self, random: &mut R, dim: usize) -> Vec<f32>
  where
    R: Rng + ?Sized,
  {
    random_vector(random, dim)
  }

  fn vector_values<R>(&self, size: usize, dimension: usize, random: &mut R) -> TestsKnnVectorValues
  where
    R: Rng + ?Sized,
  {
    let values = create_random_float_vectors(size, dimension, random);
    TestsKnnVectorValues::B(MockVectorValues::from_values(values, random.random()))
  }

  fn vector_values_from_values<R>(
    &self,
    values: Vec<Vec<f32>>,
    random: &mut R,
  ) -> TestsKnnVectorValues
  where
    R: Rng + ?Sized,
  {
    TestsKnnVectorValues::B(MockVectorValues::from_values(values, random.random()))
  }

  fn vector_values_from_reader<LR, R>(
    &self,
    reader: &LR,
    field_name: &str,
    random: &mut R,
  ) -> Result<TestsKnnVectorValues>
  where
    LR: LeafReader,
    R: Rng + ?Sized,
  {
    let vector_values = reader
      .get_float_vector_values(field_name)?
      .expect("float vector values should exist");
    let mut vectors = vec![Vec::new(); reader.max_doc()? as usize];
    for i in 0..vector_values.size() {
      let ord = vector_values.ord_to_doc(i)?;
      let value = vector_values.vector_value(i)?;
      let floats = value.as_ref();
      let dimension = vector_values.dimension();
      debug_assert!(dimension <= floats.len());
      let _ = &floats[..dimension];
      vectors[ord] = match VectorValueEnum::from(Into::<
        crate::core::index::float_vector_values::FloatVectorValue<'_>,
      >::into(value))
      {
        VectorValueEnum::Float(mut vector) => {
          vector.truncate(dimension);
          vector
        },
        value => value.as_floats()?[..dimension].to_vec(),
      };
    }
    Ok(TestsKnnVectorValues::B(MockVectorValues::from_values(
      vectors,
      random.random(),
    )))
  }

  fn vector_values_with_pregenerated<R>(
    &self,
    size: usize,
    dimension: usize,
    pregenerated_vector_values: TestsKnnVectorValues,
    pregenerated_offset: usize,
    random: &mut R,
  ) -> TestsKnnVectorValues
  where
    R: Rng + ?Sized,
  {
    let pregenerated_size = pregenerated_vector_values.size();
    let random_vectors = create_random_float_vectors(size - pregenerated_size, dimension, random);
    let mut vectors = vec![Vec::new(); size];
    let pregenerated_values = match pregenerated_vector_values {
      KnnVectorValuesEnm2::A(_) => unreachable!("unexpected byte vector values"),
      KnnVectorValuesEnm2::B(float_vector_values) => {
        Arc::unwrap_or_clone(float_vector_values.values)
      },
    };

    vectors[..pregenerated_offset].clone_from_slice(&random_vectors[..pregenerated_offset]);

    for (current_ord, value) in pregenerated_values.into_iter().enumerate() {
      vectors[pregenerated_offset + current_ord] = value;
    }

    for (dst, value) in vectors[(pregenerated_offset + pregenerated_size)..]
      .iter_mut()
      .zip(random_vectors.into_iter().skip(pregenerated_offset))
    {
      *dst = value;
    }

    TestsKnnVectorValues::B(MockVectorValues::from_values(vectors, random.random()))
  }

  fn knn_vector_field(
    &self,
    name: &str,
    vector: Vec<f32>,
    similarity_function: VectorSimilarityFunction,
  ) -> Result<Field> {
    let field_type =
      KnnFloatVectorField::create_field_type(vector.len() as i32, similarity_function)?;
    Ok(Field::new(
      name.to_owned(),
      VectorValueEnum::Float(vector),
      field_type,
    ))
  }

  fn circular_vector_values(&self, n_doc: usize) -> TestsCircularKnnVectorValues {
    KnnVectorValuesEnm2::B(CircularFloatVectorValues::new(n_doc))
  }

  fn get_target_vector(&self) -> Vec<f32> {
    vec![1.0, 0.0]
  }

  fn build_scorer<B, F>(
    &self,
    vectors: KnnVectorValuesEnm2<B, F>,
    query: Vec<f32>,
  ) -> Result<RandomVectorScorerEnum2<ByteVectorScorer<B>, FloatVectorScorer<F>>>
  where
    B: ByteVectorValues,
    F: FloatVectorValues,
  {
    match vectors {
      KnnVectorValuesEnm2::A(_) => unreachable!("unexpected byte vector values"),
      KnnVectorValuesEnm2::B(float_vector_values) => {
        let v = self.flat_vector_scorer().get_random_vector_scorer_f32(
          VectorSimilarityFunction::DotProduct,
          float_vector_values,
          query,
        )?;
        Ok(RandomVectorScorerEnum2::B(v))
      },
    }
  }

  fn vector_value(&self, vectors: &TestsKnnVectorValues, ord: usize) -> Result<Vec<f32>> {
    match vectors {
      KnnVectorValuesEnm2::A(_) => unreachable!("unexpected byte vector values"),
      KnnVectorValuesEnm2::B(float_vector_values) => {
        match VectorValueEnum::from(float_vector_values.vector_value(ord)?) {
          VectorValueEnum::Float(v) => Ok(v),
          _ => unreachable!("unexpected vector value"),
        }
      },
    }
  }
}

#[test]
fn test_search_with_skewed_accept_ords() -> Result<()> {
  run_case(|case, random| {
    let n_doc = 1000;
    case.set_similarity_function(VectorSimilarityFunction::Euclidean);
    let vectors = case.circular_vector_values(n_doc);
    let scorer_supplier = case.build_scorer_supplier(vectors.try_clone()?, random)?;
    let mut builder = create(scorer_supplier, 16, 100, random.random::<u64>())?;
    let hnsw = builder.build(vectors.size())?;

    // Skip over half of the documents that are closest to the query vector.
    let mut accept_ords = FixedBitSet::new(n_doc);
    for i in 500..n_doc {
      crate::core::util::bit_set::BitSet::set(&mut accept_ords, i)?;
    }

    let scorer = match vectors {
      KnnVectorValuesEnm2::A(_) => unreachable!("unexpected byte vector values"),
      KnnVectorValuesEnm2::B(float_vector_values) => {
        case.flat_vector_scorer().get_random_vector_scorer_f32(
          VectorSimilarityFunction::Euclidean,
          float_vector_values,
          case.get_target_vector(),
        )?
      },
    };

    let mut nn = search_with_top_k(&scorer, 10, hnsw, Some(&accept_ords), usize::MAX)?;

    let top_docs = KnnCollector::top_docs(&mut nn)?;
    assert_eq!(
      10,
      top_docs.score_docs.len(),
      "Number of found results is not equal to [10]."
    );

    let mut sum = 0_i32;
    for node in &top_docs.score_docs {
      assert!(
        crate::core::util::bits::Bits::get(&accept_ords, node.doc as usize)?,
        "the results include a deleted document: {node:?}"
      );
      sum += node.doc;
    }

    // We still expect to get reasonable recall. The lowest non-skipped docIds
    // are closest to the query vector: sum(500,509) = 5045.
    assert!(sum < 5100, "sum(result docs)={sum}");

    Ok(())
  })
}
