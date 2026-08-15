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
use crate::core::codecs::hnsw::default_flat_vector_scorer::DefaultFlatVectorScorer;
use crate::core::codecs::hnsw::flat_vector_scorer_util::GET_LUCENE99_FLAT_VECTORS_SCORER;
use crate::core::codecs::hnsw::flat_vectors_scorer::FlatVectorsScorer;
use crate::core::codecs::lucene95::off_heap_byte_vector_values::DenseOffHeapVectorValues as DenseOffHeapByteVectorValues;
use crate::core::codecs::lucene95::off_heap_float_vector_values::DenseOffHeapVectorValues as DenseOffHeapFloatVectorValues;
use crate::core::index::knn_vector_values::KnnVectorValuesEnm2;
use crate::core::index::vector_similarity_function::VectorSimilarityFunction;
use crate::core::store::data_output::DataOutput;
use crate::core::store::directory::{DirEnum, Directory};
use crate::core::store::index_input::IndexInput;
use crate::core::store::io_context::IO_CONTEXT_DEFAULT;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::hnsw::random_vector_scorer::RandomVectorScorer;
use crate::core::util::hnsw::random_vector_scorer_supplier::RandomVectorScorerSupplier;
use crate::test_framework::core::util::lucene_test_case::{new_directory_shared, random};
use rand_chacha::rand_core::Rng;
use std::sync::Arc;

#[allow(dead_code)] // for quick search
struct TestFlatVectorScorer;

type TestIndexInput = <DirEnum as Directory>::IndexInput;
type TestByteVectorValues = DenseOffHeapByteVectorValues<TestIndexInput, FlatVectorsScores>;
type TestFloatVectorValues = DenseOffHeapFloatVectorValues<TestIndexInput, FlatVectorsScores>;
type FlatVectorsScores = DefaultFlatVectorScorer;
fn set_up<R>(random: &mut R) -> Result<(Vec<FlatVectorsScores>, Arc<DirEnum>)>
where
  R: Rng + ?Sized,
{
  let scorers = vec![
    DefaultFlatVectorScorer,
    GET_LUCENE99_FLAT_VECTORS_SCORER.clone(),
  ];
  let dir = new_directory_shared(random)?;
  Ok((scorers, dir))
}

#[test]
fn test_default_or_mem_seg_scorer() -> Result<()> {
  let mut random = random();
  let (scorers, _dir) = set_up(&mut random)?;
  for scorer in scorers {
    assert_eq!(scorer.to_string(), "DefaultFlatVectorScorer()");
  }
  Ok(())
}

#[test]
fn test_multiple_byte_scorers() -> Result<()> {
  let vec0 = [0_u8, 0, 0, 0];
  let vec1 = [1_u8, 1, 1, 1];
  let vec2 = [15_u8, 15, 15, 15];
  let mut random = random();
  let (scorers, dir) = set_up(&mut random)?;

  for (index, scorer) in scorers.into_iter().enumerate() {
    let file_name = format!("test_multiple_byte_scorers_{index}");

    {
      let mut output = dir.create_output(&file_name, &IO_CONTEXT_DEFAULT)?;
      let bytes = concat_bytes(&[&vec0, &vec1, &vec2]);
      output.write_bytes_with_len(&bytes, bytes.len())?;
    }

    let input = dir.open_input(&file_name, &IO_CONTEXT_DEFAULT)?;
    let vector_values = byte_vector_values(
      4,
      3,
      &input,
      scorer.clone(),
      VectorSimilarityFunction::Euclidean,
    )?;
    let supplier = scorer
      .get_random_vector_scorer_supplier::<TestByteVectorValues, TestFloatVectorValues>(
        VectorSimilarityFunction::Euclidean,
        KnnVectorValuesEnm2::A(vector_values),
      )?;

    let scorer_against_ord0 = supplier.scorer(0)?;
    let first_score = scorer_against_ord0.score(1)?;
    let _scorer_against_ord2 = supplier.scorer(2)?;
    let score_again = scorer_against_ord0.score(1)?;

    assert_eq!(score_again, first_score);
  }

  Ok(())
}

#[test]
fn test_multiple_float_scorers() -> Result<()> {
  let vec0 = [0.0_f32, 0.0, 0.0, 0.0];
  let vec1 = [1.0_f32, 1.0, 1.0, 1.0];
  let vec2 = [15.0_f32, 15.0, 15.0, 15.0];
  let mut random = random();
  let (scorers, dir) = set_up(&mut random)?;

  for (index, scorer) in scorers.into_iter().enumerate() {
    let file_name = format!("test_multiple_float_scorers_{index}");

    {
      let mut output = dir.create_output(&file_name, &IO_CONTEXT_DEFAULT)?;
      let bytes = concat_f32(&[&vec0, &vec1, &vec2]);
      output.write_bytes_with_len(&bytes, bytes.len())?;
    }

    let input = dir.open_input(&file_name, &IO_CONTEXT_DEFAULT)?;
    let vector_values = float_vector_values(
      4,
      3,
      &input,
      scorer.clone(),
      VectorSimilarityFunction::Euclidean,
    )?;
    let supplier = scorer
      .get_random_vector_scorer_supplier::<TestByteVectorValues, TestFloatVectorValues>(
        VectorSimilarityFunction::Euclidean,
        KnnVectorValuesEnm2::B(vector_values),
      )?;

    let scorer_against_ord0 = supplier.scorer(0)?;
    let first_score = scorer_against_ord0.score(1)?;
    let _scorer_against_ord2 = supplier.scorer(2)?;
    let score_again = scorer_against_ord0.score(1)?;

    assert_eq!(score_again, first_score);
  }

  Ok(())
}

#[test]
fn test_check_byte_dimensions() -> Result<()> {
  let similarities = [
    VectorSimilarityFunction::Cosine,
    VectorSimilarityFunction::DotProduct,
    VectorSimilarityFunction::Euclidean,
    VectorSimilarityFunction::MaximumInnerProduct,
  ];
  let mut random = random();
  let (scorers, dir) = set_up(&mut random)?;

  for (index, scorer) in scorers.into_iter().enumerate() {
    let file_name = format!("test_check_byte_dimensions_{index}");

    {
      let mut output = dir.create_output(&file_name, &IO_CONTEXT_DEFAULT)?;
      let vec0 = [0_u8; 4];
      output.write_bytes_with_len(&vec0, vec0.len())?;
    }

    let input = dir.open_input(&file_name, &IO_CONTEXT_DEFAULT)?;
    for similarity in similarities {
      let vector_values = byte_vector_values(4, 1, &input, scorer.clone(), similarity)?;
      let result = scorer.get_random_vector_scorer_u8(similarity, vector_values, vec![0_u8; 5]);
      assert!(matches!(result, Err(LuceneError::IllegalArgument(_))));
    }
  }

  Ok(())
}

#[test]
fn test_check_float_dimensions() -> Result<()> {
  let similarities = [
    VectorSimilarityFunction::Cosine,
    VectorSimilarityFunction::DotProduct,
    VectorSimilarityFunction::Euclidean,
    VectorSimilarityFunction::MaximumInnerProduct,
  ];
  let mut random = random();
  let (scorers, dir) = set_up(&mut random)?;

  for (index, scorer) in scorers.into_iter().enumerate() {
    let file_name = format!("test_check_float_dimensions_{index}");

    {
      let mut output = dir.create_output(&file_name, &IO_CONTEXT_DEFAULT)?;
      let bytes = concat_f32(&[&[0.0_f32, 0.0, 0.0, 0.0]]);
      output.write_bytes_with_len(&bytes, bytes.len())?;
    }

    let input = dir.open_input(&file_name, &IO_CONTEXT_DEFAULT)?;
    for similarity in similarities {
      let vector_values = float_vector_values(4, 1, &input, scorer.clone(), similarity)?;
      let result = scorer.get_random_vector_scorer_f32(similarity, vector_values, vec![0.0_f32; 5]);
      assert!(matches!(result, Err(LuceneError::IllegalArgument(_))));
    }
  }

  Ok(())
}
fn byte_vector_values(
  dims: usize,
  size: usize,
  input: &TestIndexInput,
  scorer: FlatVectorsScores,
  similarity_function: VectorSimilarityFunction,
) -> Result<TestByteVectorValues> {
  Ok(DenseOffHeapByteVectorValues::new(
    dims,
    size,
    input.slice("byte_values", 0, input.length()?)?,
    dims,
    scorer,
    similarity_function,
  ))
}

fn float_vector_values(
  dims: usize,
  size: usize,
  input: &TestIndexInput,
  scorer: FlatVectorsScores,
  similarity_function: VectorSimilarityFunction,
) -> Result<TestFloatVectorValues> {
  DenseOffHeapFloatVectorValues::new(
    dims,
    size,
    input.slice("float_values", 0, input.length()?)?,
    dims * BitUtil::FLOAT_BYTES,
    scorer,
    similarity_function,
  )
}

fn concat_f32(arrays: &[&[f32]]) -> Vec<u8> {
  let total_len = arrays
    .iter()
    .map(|array| std::mem::size_of_val(*array))
    .sum();
  let mut result = Vec::with_capacity(total_len);
  for array in arrays {
    for value in *array {
      result.extend_from_slice(&value.to_le_bytes());
    }
  }
  result
}
fn concat_bytes(arrays: &[&[u8]]) -> Vec<u8> {
  let total_len = arrays.iter().map(|array| array.len()).sum();
  let mut result = Vec::with_capacity(total_len);
  for array in arrays {
    result.extend_from_slice(array);
  }
  result
}
