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
use crate::core::codecs::codec_formats::CodecKnnVectorsReaderInner;
use crate::core::codecs::knn_field_vectors_writer::VectorValueEnum;
use crate::core::codecs::lucene99::lucene99_scalar_quantized_vectors_format::Lucene99ScalarQuantizedVectorsFormat;
use crate::core::codecs::lucene99::lucene99_scalar_quantized_vectors_writer::{
  FloatVectorWrapper, build_scalar_quantizer,
};
use crate::core::codecs::{Codecs, KnnVectorsFormats, KnnVectorsFormatsReader};
use crate::core::document::document::Document;
use crate::core::document::knn_float_vector_field::KnnFloatVectorField;
use crate::core::index::byte_vector_values::ByteVectorValues;
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::directory_reader;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::{DISABLE_AUTO_FLUSH, IndexWriterConfig};
use crate::core::index::knn_vector_values::{DocIndexIterator, KnnVectorValues};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::no_merge_policy::NoMergePolicy;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::index::vector_similarity_function::VectorSimilarityFunction;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::knn_collector::KnnCollector;
use crate::core::search::top_knn_collector::TopKnnCollector;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::quantization::quantized_byte_vector_values::QuantizedByteVectorValues;
use crate::core::util::quantization::quantized_vectors_reader::QuantizedVectorsReader;
use crate::core::util::vector_util::VectorUtil;
use crate::test_framework::core::codecs::asserting_codec::AssertingCodecKnnVectorsReader;
use crate::test_framework::core::index::base_index_file_format_test_case::BaseIndexFileFormatTestCase;
use crate::test_framework::core::index::base_knn_vectors_format_test_case::{
  BaseKnnVectorsFormatTestCase, BaseKnnVectorsFormatTestCaseState,
};
use crate::test_framework::core::util::lucene_test_case::{
  get_only_leaf_reader, new_directory_shared, new_index_writer_config, random,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::prelude::StdRng;
use rand::{Rng, RngExt};

#[allow(dead_code)] // for quick search
pub struct TestLucene99ScalarQuantizedVectorsFormat {
  format: KnnVectorsFormats,
  confidence_interval: Option<f32>,
  bits: i32,
  base_knn_vectors_format_test_case_state: BaseKnnVectorsFormatTestCaseState,
}

impl TestLucene99ScalarQuantizedVectorsFormat {
  fn new<R>(random: &mut R) -> Result<Self>
  where
    R: Rng + ?Sized,
  {
    let base_knn_vectors_format_test_case_state = BaseKnnVectorsFormatTestCaseState::new(random);
    let bits = if random.random_bool(0.5) { 4 } else { 7 };
    let mut confidence_interval = random
      .random_bool(0.5)
      .then(|| random.random_range(0.9_f32..1.0_f32));
    if random.random_bool(0.5) {
      confidence_interval = Some(0.0);
    }
    let compress = bits == 4 && random.random_bool(0.5);
    Ok(Self {
      format: Lucene99ScalarQuantizedVectorsFormat::with_confidence_interval_bits_compress(
        confidence_interval,
        bits,
        compress,
      )?
      .into(),
      confidence_interval,
      bits,
      base_knn_vectors_format_test_case_state,
    })
  }
}

#[test]
fn test_search() -> Result<()> {
  run_case(|_case, random| {
    let dir = new_directory_shared(random)?;
    let writer = IndexWriter::new(dir.clone(), new_index_writer_config(random)?)?;
    let mut doc = Document::new();
    // randomly reuse a vector, this ensures the underlying codec doesn't rely on the array
    // reference
    doc.add(KnnFloatVectorField::with_similarity_function(
      "f",
      vec![0.0, 1.0],
      VectorSimilarityFunction::DotProduct,
    )?);
    writer.add_document(doc)?;
    writer.commit()?;

    {
      let reader = directory_reader::open_from_writer(&writer)?;
      let leaf = get_only_leaf_reader(reader)?;
      let mut collector = TopKnnCollector::new(1, i32::MAX as usize)?;
      LeafReader::search_nearest_vectors_f32(
        &leaf,
        "f",
        vec![1.0, 0.0],
        &mut collector,
        leaf.get_live_docs()?,
      )?;
      assert!(collector.top_docs()?.score_docs.is_empty());
    }

    writer.close()?;
    dir.close()
  })
}

#[test]
fn test_quantized_vectors_write_and_read() -> Result<()> {
  run_case(|case, random| {
    let num_vectors = random.random_range(1..=50);
    let similarity_function = case.random_similarity(random);
    let normalize = similarity_function == VectorSimilarityFunction::Cosine;
    let mut dim: usize = random.random_range(1..=64);
    if !dim.is_multiple_of(2) {
      dim += 1;
    }
    let vectors = (0..num_vectors)
      .map(|_| {
        VectorValueEnum::Float(TestLucene99ScalarQuantizedVectorsFormat::random_vector(
          random, dim,
        ))
      })
      .collect::<Vec<_>>();
    let scalar_quantizer = build_scalar_quantizer(
      FloatVectorWrapper::new(&vectors),
      num_vectors,
      similarity_function,
      case.confidence_interval,
      case.bits as u8,
    )?;
    let mut expected_corrections = vec![0.0; num_vectors];
    let mut expected_vectors = vec![vec![0; dim]; num_vectors];
    for (i, vector) in vectors.iter().enumerate() {
      let mut vector = vector.as_floats()?.to_vec();
      if normalize {
        VectorUtil::l2normalize(&mut vector)?;
      }
      expected_corrections[i] =
        scalar_quantizer.quantize(&vector, &mut expected_vectors[i], similarity_function);
    }
    let mut randomly_reused_vector = vec![0.0; dim];

    let dir = new_directory_shared(random)?;
    let mut config = IndexWriterConfig::new()?;
    config.set_codec(case.get_codec()?);
    config.set_max_buffered_docs(num_vectors as i32 + 1);
    config.set_ram_buffer_size_mb(DISABLE_AUTO_FLUSH as f64);
    config.set_merge_policy(NoMergePolicy::default());
    let writer = IndexWriter::new(dir.clone(), config)?;
    for vector in &vectors {
      let vector = vector.as_floats()?;
      let value = if random.random_bool(0.5) {
        randomly_reused_vector.copy_from_slice(vector);
        randomly_reused_vector.clone()
      } else {
        vector.to_vec()
      };
      let mut doc = Document::new();
      doc.add(KnnFloatVectorField::with_similarity_function(
        "f",
        value,
        similarity_function,
      )?);
      writer.add_document(doc)?;
    }
    writer.commit()?;

    {
      let reader = directory_reader::open_from_writer(&writer)?;
      let leaf = get_only_leaf_reader(reader)?;
      let vector_reader = leaf
        .get_vector_reader()?
        .expect("vector reader should exist");
      let fields_reader = match vector_reader.as_ref().as_inner() {
        CodecKnnVectorsReaderInner::Asserting(fields_reader) => fields_reader,
        _ => panic!("reader is not the Asserting codec's per-field KNN reader"),
      };
      let field_reader = fields_reader
        .get_field_reader("f")?
        .expect("field reader should exist");
      let source_reader = match field_reader.as_ref() {
        AssertingCodecKnnVectorsReader::Source(source_reader) => source_reader,
        _ => panic!("field reader is not a source KNN format reader"),
      };
      let quantized_reader = match source_reader {
        KnnVectorsFormatsReader::Lucene99ScalarQuantized(reader) => reader,
        _ => panic!("reader is not Lucene99ScalarQuantizedVectorsReader"),
      };
      assert!(quantized_reader.get_quantization_state("f")?.is_some());
      let quantized_values = quantized_reader.get_quantized_vector_values("f")?;
      let mut iterator = quantized_values.iterator()?;
      let mut doc_id = iterator.next_doc()?;
      while doc_id != crate::core::search::doc_id_set_iterator::NO_MORE_DOCS {
        let ord = iterator.index()? as usize;
        let vector = quantized_values.vector_value(ord)?;
        assert_eq!(
          expected_vectors[doc_id as usize],
          vector.as_ref().as_bytes()?
        );
        let correction = quantized_values.get_score_correction_constant(ord)?;
        assert!((expected_corrections[doc_id as usize] - correction).abs() <= 0.00001);
        doc_id = iterator.next_doc()?;
      }
    }

    writer.close()?;
    dir.close()
  })
}

#[test]
fn test_to_string() -> Result<()> {
  let format = Lucene99ScalarQuantizedVectorsFormat::with_confidence_interval_bits_compress(
    Some(0.9),
    4,
    false,
  )?;
  assert_eq!(
    "Lucene99ScalarQuantizedVectorsFormat(name=Lucene99ScalarQuantizedVectorsFormat, confidenceInterval=0.9, bits=4, compress=false, flatVectorScorer=ScalarQuantizedVectorScorer(nonQuantizedDelegate=DefaultFlatVectorScorer()), rawVectorFormat=Lucene99FlatVectorsFormat(vectorsScorer=DefaultFlatVectorScorer()))",
    format.to_string()
  );
  Ok(())
}

#[test]
fn test_limits() -> Result<()> {
  assert!(matches!(
    Lucene99ScalarQuantizedVectorsFormat::with_confidence_interval_bits_compress(
      Some(1.1),
      7,
      false
    ),
    Err(LuceneError::IllegalArgument(_))
  ));
  assert!(matches!(
    Lucene99ScalarQuantizedVectorsFormat::with_confidence_interval_bits_compress(None, -1, false),
    Err(LuceneError::IllegalArgument(_))
  ));
  assert!(matches!(
    Lucene99ScalarQuantizedVectorsFormat::with_confidence_interval_bits_compress(None, 5, false),
    Err(LuceneError::IllegalArgument(_))
  ));
  assert!(matches!(
    Lucene99ScalarQuantizedVectorsFormat::with_confidence_interval_bits_compress(None, 9, false),
    Err(LuceneError::IllegalArgument(_))
  ));
  Ok(())
}

#[test]
fn test_random_with_updates_and_graph() -> Result<()> {
  // graph not supported
  Ok(())
}

#[test]
fn test_search_with_visited_limit() -> Result<()> {
  // search not supported
  Ok(())
}

mod base_knn_vectors_format_test_case_tests {
  use crate::core::util::error::lucene_error::Result;
  use crate::test::core::codecs::lucene99::test_lucene99_scalar_quantized_vectors_format::run_case;
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

impl BaseIndexFileFormatTestCase for TestLucene99ScalarQuantizedVectorsFormat {
  type Defaults = crate::test_framework::core::index::base_knn_vectors_format_test_case::BaseKnnVectorsFormatTestCaseDefaults;

  fn get_codec(&self) -> Result<Codecs> {
    Ok(TestUtil::always_knn_vectors_format(self.format.clone()).into())
  }
}

impl BaseKnnVectorsFormatTestCase for TestLucene99ScalarQuantizedVectorsFormat {
  fn base_knn_vectors_format_test_case_state(&self) -> &BaseKnnVectorsFormatTestCaseState {
    &self.base_knn_vectors_format_test_case_state
  }
}

fn run_case<F>(f: F) -> Result<()>
where
  F: FnOnce(&TestLucene99ScalarQuantizedVectorsFormat, &mut StdRng) -> Result<()>,
{
  let mut random = random();
  let case = TestLucene99ScalarQuantizedVectorsFormat::new(&mut random)?;
  let codec_guard = case.set_up()?;
  let result = f(&case, &mut random);
  case.tear_down(codec_guard);
  result
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
