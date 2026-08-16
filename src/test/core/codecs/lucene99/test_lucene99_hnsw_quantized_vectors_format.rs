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
use crate::core::codecs::lucene99::lucene99_hnsw_scalar_quantized_vectors_format::Lucene99HnswScalarQuantizedVectorsFormat;
use crate::core::codecs::lucene99::lucene99_hnsw_vectors_format::{
  DEFAULT_BEAM_WIDTH, DEFAULT_MAX_CONN, MAXIMUM_BEAM_WIDTH, MAXIMUM_MAX_CONN,
};
use crate::core::codecs::lucene99::lucene99_hnsw_vectors_reader::SIMILARITY_FUNCTIONS;
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
use crate::core::index::knn_vector_values::KnnVectorValues;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::no_merge_policy::NoMergePolicy;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::index::vector_similarity_function::VectorSimilarityFunction;
use crate::core::search::knn_collector::KnnCollector;
use crate::core::search::knn_float_vector_query::KnnFloatVectorQuery;
use crate::core::search::top_docs::TopDocsLike;
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
  get_only_leaf_reader, new_directory_shared, new_index_writer_config, new_searcher_with_reader,
  random,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::prelude::StdRng;
use rand::{Rng, RngExt};

#[allow(dead_code)] // for quick search
struct TestLucene99HnswQuantizedVectorsFormat {
  format: KnnVectorsFormats,
  confidence_interval: Option<f32>,
  bits: i32,
  base_knn_vectors_format_test_case_state: BaseKnnVectorsFormatTestCaseState,
}

impl TestLucene99HnswQuantizedVectorsFormat {
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
    Ok(Self {
      format: Self::get_knn_format(random, bits, confidence_interval)?.into(),
      confidence_interval,
      bits,
      base_knn_vectors_format_test_case_state,
    })
  }

  fn get_knn_format<R>(
    random: &mut R,
    bits: i32,
    confidence_interval: Option<f32>,
  ) -> Result<Lucene99HnswScalarQuantizedVectorsFormat>
  where
    R: Rng + ?Sized,
  {
    Lucene99HnswScalarQuantizedVectorsFormat::
      with_graph_para_with_threads_bits_compress_confidence_interval(
        DEFAULT_MAX_CONN,
        DEFAULT_BEAM_WIDTH,
        1,
        bits,
        bits == 4 && random.random_bool(0.5),
        confidence_interval,
      )
  }
}

// Verifies it's fine to change your mind on the number of bits quantization you want for the same
// field in the same index by changing up the Codec. This is allowed because at merge time we
// requantize the vectors.
#[test]
fn test_mixed_quantized_bits() -> Result<()> {
  let mut random = random();
  let case = TestLucene99HnswQuantizedVectorsFormat::new(&mut random)?;
  let dir = new_directory_shared(&mut random)?;

  // add first vector using 4 bit quantization, then close index:
  let mut config = new_index_writer_config(&mut random)?;
  config.set_codec(TestUtil::always_knn_vectors_format(
    TestLucene99HnswQuantizedVectorsFormat::get_knn_format(
      &mut random,
      4,
      case.confidence_interval,
    )?,
  ));
  let writer = IndexWriter::new(dir.clone(), config)?;
  let mut doc = Document::new();
  doc.add(KnnFloatVectorField::with_similarity_function(
    "f",
    vec![0.6, 0.8],
    VectorSimilarityFunction::DotProduct,
  )?);
  writer.add_document(doc)?;
  writer.close()?;

  // create another writer using 7 bit quantization and add 2nd vector
  let mut config = new_index_writer_config(&mut random)?;
  config.set_codec(TestUtil::always_knn_vectors_format(
    TestLucene99HnswQuantizedVectorsFormat::get_knn_format(
      &mut random,
      7,
      case.confidence_interval,
    )?,
  ));
  let writer = IndexWriter::new(dir.clone(), config)?;
  let mut doc = Document::new();
  doc.add(KnnFloatVectorField::with_similarity_function(
    "f",
    vec![0.8, 0.6],
    VectorSimilarityFunction::DotProduct,
  )?);
  writer.add_document(doc)?;
  writer.force_merge(1)?;
  writer.close()?;

  // confirm searching works: we find both vectors
  {
    let searcher = new_searcher_with_reader(directory_reader::open(dir.clone())?)?;
    let query = KnnFloatVectorQuery::new("f", vec![0.7, 0.7], 10)?;
    let top_docs = searcher.search(query, 100)?;
    assert_eq!(2, top_docs.total_hits().value());
  }
  dir.close()?;
  Ok(())
}

// Verifies you can change your mind and enable quantization on a previously indexed vector field
// without quantization.
#[test]
fn test_mixed_quantized_un_quantized() -> Result<()> {
  let mut random = random();
  let case = TestLucene99HnswQuantizedVectorsFormat::new(&mut random)?;
  let dir = new_directory_shared(&mut random)?;

  // add first vector using no quantization
  let mut config = new_index_writer_config(&mut random)?;
  config.set_codec(TestUtil::always_knn_vectors_format(
    crate::core::codecs::lucene99::lucene99_hnsw_vectors_format::Lucene99HnswVectorsFormat::new()?,
  ));
  let writer = IndexWriter::new(dir.clone(), config)?;
  let mut doc = Document::new();
  doc.add(KnnFloatVectorField::with_similarity_function(
    "f",
    vec![0.6, 0.8],
    VectorSimilarityFunction::DotProduct,
  )?);
  writer.add_document(doc)?;
  writer.close()?;

  // create another writer using (7 bit) quantization and add 2nd vector
  let mut config = new_index_writer_config(&mut random)?;
  config.set_codec(TestUtil::always_knn_vectors_format(
    TestLucene99HnswQuantizedVectorsFormat::get_knn_format(
      &mut random,
      7,
      case.confidence_interval,
    )?,
  ));
  let writer = IndexWriter::new(dir.clone(), config)?;
  let mut doc = Document::new();
  doc.add(KnnFloatVectorField::with_similarity_function(
    "f",
    vec![0.8, 0.6],
    VectorSimilarityFunction::DotProduct,
  )?);
  writer.add_document(doc)?;
  writer.force_merge(1)?;
  writer.close()?;

  // confirm searching works: we find both vectors
  {
    let searcher = new_searcher_with_reader(directory_reader::open(dir.clone())?)?;
    let query = KnnFloatVectorQuery::new("f", vec![0.7, 0.7], 10)?;
    let top_docs = searcher.search(query, 100)?;
    assert_eq!(2, top_docs.total_hits().value());
  }
  dir.close()?;
  Ok(())
}

#[test]
fn test_quantization_scoring_edge_case() -> Result<()> {
  let mut random = random();
  let vectors = [vec![0.6, 0.8], vec![0.8, 0.6], vec![-0.6, -0.8]];
  let dir = new_directory_shared(&mut random)?;
  let mut config = new_index_writer_config(&mut random)?;
  config.set_codec(TestUtil::always_knn_vectors_format(
    Lucene99HnswScalarQuantizedVectorsFormat::
      with_graph_para_with_threads_bits_compress_confidence_interval(
        16,
        100,
        1,
        7,
        false,
        Some(0.9),
      )?,
  ));
  let writer = IndexWriter::new(dir.clone(), config)?;
  for vector in vectors {
    let mut doc = Document::new();
    doc.add(KnnFloatVectorField::with_similarity_function(
      "f",
      vector,
      VectorSimilarityFunction::DotProduct,
    )?);
    writer.add_document(doc)?;
    writer.commit()?;
  }
  writer.force_merge(1)?;
  {
    let reader = directory_reader::open_from_writer(&writer)?;
    let leaf = get_only_leaf_reader(reader)?;
    let mut collector = TopKnnCollector::new(5, i32::MAX as usize)?;
    LeafReader::search_nearest_vectors_f32(
      &leaf,
      "f",
      vec![0.6, 0.8],
      &mut collector,
      leaf.get_live_docs()?,
    )?;
    let top_docs = collector.top_docs()?;
    assert_eq!(3, top_docs.total_hits.value());
    for score_doc in top_docs.score_docs {
      assert!(score_doc.score >= 0.0);
    }
  }
  writer.close()?;
  dir.close()?;
  Ok(())
}

#[test]
fn test_quantized_vectors_write_and_read() -> Result<()> {
  run_case(|case, random| {
    let num_vectors = random.random_range(1..=50);
    let similarity_function = case.random_similarity(random);
    let mut dim: usize = random.random_range(1..=64);
    if !dim.is_multiple_of(2) {
      dim += 1;
    }
    let vectors = (0..num_vectors)
      .map(|_| {
        VectorValueEnum::Float(TestLucene99HnswQuantizedVectorsFormat::random_vector(
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
      if similarity_function == VectorSimilarityFunction::Cosine {
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
        KnnVectorsFormatsReader::Lucene99HnswScalarQuantized(reader) => reader,
        _ => panic!("reader is not Lucene99HnswVectorsReader"),
      };
      assert!(quantized_reader.get_quantization_state("f")?.is_some());
      let quantized_values = quantized_reader.get_quantized_vector_values("f")?;
      for ord in 0..quantized_values.size() {
        let vector = quantized_values.vector_value(ord)?;
        assert_eq!(expected_vectors[ord], vector.as_ref().as_bytes()?);
        let correction = quantized_values.get_score_correction_constant(ord)?;
        assert!((expected_corrections[ord] - correction).abs() <= 0.00001);
      }
    }

    writer.close()?;
    dir.close()
  })
}

#[test]
fn test_to_string() -> Result<()> {
  let format = Lucene99HnswScalarQuantizedVectorsFormat::
    with_graph_para_with_threads_bits_compress_confidence_interval(
      10,
      20,
      1,
      4,
      false,
      Some(0.9),
    )?;
  let expected = "Lucene99HnswScalarQuantizedVectorsFormat(name=Lucene99HnswScalarQuantizedVectorsFormat, maxConn=10, beamWidth=20, flatVectorFormat=Lucene99ScalarQuantizedVectorsFormat(name=Lucene99ScalarQuantizedVectorsFormat, confidenceInterval=0.9, bits=4, compress=false, flatVectorScorer=ScalarQuantizedVectorScorer(nonQuantizedDelegate=DefaultFlatVectorScorer()), rawVectorFormat=Lucene99FlatVectorsFormat(vectorsScorer=DefaultFlatVectorScorer())))";
  assert_eq!(expected, format.to_string());
  Ok(())
}

#[test]
fn test_limits() -> Result<()> {
  // TODO: The Java -1 maxConn and beamWidth cases cannot be represented by Rust's usize
  // constructor parameters.
  assert!(matches!(
    Lucene99HnswScalarQuantizedVectorsFormat::with_graph_para(0, 20),
    Err(LuceneError::IllegalArgument(_))
  ));
  assert!(matches!(
    Lucene99HnswScalarQuantizedVectorsFormat::with_graph_para(20, 0),
    Err(LuceneError::IllegalArgument(_))
  ));
  assert!(matches!(
    Lucene99HnswScalarQuantizedVectorsFormat::with_graph_para(MAXIMUM_MAX_CONN + 1, 20),
    Err(LuceneError::IllegalArgument(_))
  ));
  assert!(matches!(
    Lucene99HnswScalarQuantizedVectorsFormat::with_graph_para(20, MAXIMUM_BEAM_WIDTH + 1),
    Err(LuceneError::IllegalArgument(_))
  ));
  assert!(matches!(
    Lucene99HnswScalarQuantizedVectorsFormat::
      with_graph_para_with_threads_bits_compress_confidence_interval(
        20,
        100,
        0,
        7,
        false,
        Some(1.1),
      ),
    Err(LuceneError::IllegalArgument(_))
  ));
  assert!(matches!(
    Lucene99HnswScalarQuantizedVectorsFormat::
      with_graph_para_with_threads_bits_compress_confidence_interval(
        20, 100, 0, -1, false, None,
      ),
    Err(LuceneError::IllegalArgument(_))
  ));
  assert!(matches!(
    Lucene99HnswScalarQuantizedVectorsFormat::
      with_graph_para_with_threads_bits_compress_confidence_interval(
        20, 100, 0, 5, false, None,
      ),
    Err(LuceneError::IllegalArgument(_))
  ));

  assert!(matches!(
    Lucene99HnswScalarQuantizedVectorsFormat::
      with_graph_para_with_threads_bits_compress_confidence_interval(
        20, 100, 0, 9, false, None,
      ),
    Err(LuceneError::IllegalArgument(_))
  ));
  assert!(matches!(
    Lucene99HnswScalarQuantizedVectorsFormat::
      with_graph_para_with_threads_bits_compress_confidence_interval(
        20,
        100,
        0,
        7,
        false,
        Some(0.8),
      ),
    Err(LuceneError::IllegalArgument(_))
  ));
  // TODO: The Rust constructor has no executor argument corresponding to Java's
  // SameThreadExecutorService rejection case.
  Ok(())
}

// Ensures that all expected vector similarity functions are translatable in the format.
#[test]
fn test_vector_similarity_funcs() {
  // This does not necessarily have to be all similarity functions, but differences should be
  // considered carefully.
  let expected_values = [
    VectorSimilarityFunction::Euclidean,
    VectorSimilarityFunction::DotProduct,
    VectorSimilarityFunction::Cosine,
    VectorSimilarityFunction::MaximumInnerProduct,
  ];
  assert_eq!(SIMILARITY_FUNCTIONS, expected_values);
}

mod base_knn_vectors_format_test_case_tests {
  use crate::core::util::error::lucene_error::Result;
  use crate::test::core::codecs::lucene99::test_lucene99_hnsw_quantized_vectors_format::run_case;
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

impl BaseIndexFileFormatTestCase for TestLucene99HnswQuantizedVectorsFormat {
  type Defaults = crate::test_framework::core::index::base_knn_vectors_format_test_case::BaseKnnVectorsFormatTestCaseDefaults;

  fn get_codec(&self) -> Result<Codecs> {
    Ok(TestUtil::always_knn_vectors_format(self.format.clone()).into())
  }
}

impl BaseKnnVectorsFormatTestCase for TestLucene99HnswQuantizedVectorsFormat {
  fn base_knn_vectors_format_test_case_state(&self) -> &BaseKnnVectorsFormatTestCaseState {
    &self.base_knn_vectors_format_test_case_state
  }
}

fn run_case<F>(f: F) -> Result<()>
where
  F: FnOnce(&TestLucene99HnswQuantizedVectorsFormat, &mut StdRng) -> Result<()>,
{
  let mut random = random();
  let case = TestLucene99HnswQuantizedVectorsFormat::new(&mut random)?;
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
