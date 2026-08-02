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
use crate::codec::bitvectors::hnsw_bit_vectors_format::HnswBitVectorsFormat;
use crate::core::codecs::Codecs;
use crate::core::codecs::lucene99::lucene99_hnsw_vectors_format::{
  MAXIMUM_BEAM_WIDTH, MAXIMUM_MAX_CONN,
};
use crate::core::document::document::Document;
use crate::core::document::field::Store;
use crate::core::document::knn_byte_vector_field::KnnByteVectorField;
use crate::core::document::knn_float_vector_field::KnnFloatVectorField;
use crate::core::document::string_field::StringField;
use crate::core::index::directory_reader;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::index::vector_similarity_function::VectorSimilarityFunction;
use crate::core::search::knn_collector::KnnCollector;
use crate::core::search::top_knn_collector::TopKnnCollector;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::vector_util::VectorUtil;
use crate::test_framework::core::index::base_index_file_format_test_case::{
  BaseIndexFileFormatTestCase, BaseIndexFileFormatTestCaseDefaults,
};
use crate::test_framework::core::util::lucene_test_case::{
  get_only_leaf_reader, new_directory_shared, random,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::{Rng, RngExt};

#[allow(dead_code)] // for quick search
struct TestHnswBitVectorsFormat;

struct TestHnswBitVectorsFormatDefaults;

impl BaseIndexFileFormatTestCaseDefaults<TestHnswBitVectorsFormat>
  for TestHnswBitVectorsFormatDefaults
{
  fn add_random_fields<R>(
    _test_case: &TestHnswBitVectorsFormat,
    random: &mut R,
    document: &mut Document,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let mut vector = vec![0.0; 30];
    let mut square_sum = 0.0;
    while square_sum == 0.0 {
      square_sum = 0.0;
      for value in &mut vector {
        *value = random.random::<f32>();
        square_sum += *value * *value;
      }
    }
    VectorUtil::l2normalize(&mut vector)?;
    let vector = vector
      .into_iter()
      .map(|value| (value * 127.0) as i8 as u8)
      .collect();
    document.add(KnnByteVectorField::with_similarity_function(
      "v2",
      vector,
      VectorSimilarityFunction::DotProduct,
    )?);
    Ok(())
  }
}

impl BaseIndexFileFormatTestCase for TestHnswBitVectorsFormat {
  type Defaults = TestHnswBitVectorsFormatDefaults;

  fn get_codec(&self) -> Result<Codecs> {
    Ok(TestUtil::always_knn_vectors_format(HnswBitVectorsFormat::new()?).into())
  }
}

fn run_case<F>(f: F) -> Result<()>
where
  F: FnOnce(&TestHnswBitVectorsFormat, &mut rand::prelude::StdRng) -> Result<()>,
{
  let mut random = random();
  let case = TestHnswBitVectorsFormat;
  f(&case, &mut random)
}

#[test]
fn test_float_vector_fails() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut config = IndexWriterConfig::new()?;
  config.set_codec(TestUtil::always_knn_vectors_format(
    HnswBitVectorsFormat::new()?,
  ));
  let writer = IndexWriter::new(dir.clone(), config)?;
  let mut doc = Document::new();
  doc.add(KnnFloatVectorField::with_similarity_function(
    "f",
    vec![0.0; 4],
    VectorSimilarityFunction::DotProduct,
  )?);
  let error = writer.add_document(doc).unwrap_err();
  assert!(matches!(error, LuceneError::IllegalArgument(_)));
  assert!(
    error
      .to_string()
      .contains("HnswBitVectorsFormat only supports BYTE encoding")
  );
  writer.close()?;
  dir.close()
}

#[test]
fn test_index_and_search_bit_vectors() -> Result<()> {
  let vectors = [
    vec![0b1010_1110, 0b0101_0111],
    vec![0b1111_1000, 0b0000_1111],
    vec![0b1100_1100, 0b0011_0011],
    vec![0b1111_1111, 0b0000_0000],
    vec![0b0000_0000, 0b0000_0000],
  ];
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut config = IndexWriterConfig::new()?;
  config.set_codec(TestUtil::always_knn_vectors_format(
    HnswBitVectorsFormat::new()?,
  ));
  let writer = IndexWriter::new(dir.clone(), config)?;
  for (id, vector) in vectors.iter().enumerate() {
    let mut doc = Document::new();
    doc.add(KnnByteVectorField::with_similarity_function(
      "v1",
      vector.clone(),
      VectorSimilarityFunction::DotProduct,
    )?);
    doc.add(StringField::from_string("id", id.to_string(), Store::Yes)?);
    writer.add_document(doc)?;
  }
  writer.commit()?;
  writer.force_merge(1)?;

  {
    let reader = directory_reader::open_from_writer(&writer)?;
    let leaf = get_only_leaf_reader(reader)?;
    let mut collector = TopKnnCollector::new(3, i32::MAX as usize)?;
    leaf.search_nearest_vectors_u8(
      "v1",
      vectors[0].clone(),
      &mut collector,
      leaf.get_live_docs()?,
    )?;
    let top_docs = collector.top_docs()?;
    assert_eq!(3, top_docs.score_docs.len());

    let mut fields = leaf.stored_fields()?;
    assert_eq!(
      "0",
      fields
        .document(top_docs.score_docs[0].doc)?
        .get("id")?
        .unwrap()
        .as_ref()
    );
    assert!((1.0 - top_docs.score_docs[0].score).abs() < 1e-12);
    assert_eq!(
      "2",
      fields
        .document(top_docs.score_docs[1].doc)?
        .get("id")?
        .unwrap()
        .as_ref()
    );
    assert!((0.625 - top_docs.score_docs[1].score).abs() < 1e-12);
    assert_eq!(
      "1",
      fields
        .document(top_docs.score_docs[2].doc)?
        .get("id")?
        .unwrap()
        .as_ref()
    );
    assert!((0.5625 - top_docs.score_docs[2].score).abs() < 1e-12);
  }

  writer.close()?;
  dir.close()
}

#[test]
fn test_to_string() -> Result<()> {
  let format = HnswBitVectorsFormat::with_graph_para(10, 20)?;
  assert_eq!(
    "HnswBitVectorsFormat(name=HnswBitVectorsFormat, maxConn=10, beamWidth=20, flatVectorFormat=Lucene99FlatVectorsFormat(vectorsScorer=FlatBitVectorsScorer()))",
    format.to_string()
  );
  Ok(())
}

#[test]
fn test_limits() -> Result<()> {
  // Rust uses usize for max_conn and beam_width, so Java's -1 cases cannot be expressed.
  assert!(matches!(
    HnswBitVectorsFormat::with_graph_para(0, 20),
    Err(LuceneError::IllegalArgument(_))
  ));
  assert!(matches!(
    HnswBitVectorsFormat::with_graph_para(20, 0),
    Err(LuceneError::IllegalArgument(_))
  ));
  assert!(matches!(
    HnswBitVectorsFormat::with_graph_para(MAXIMUM_MAX_CONN + 1, 20),
    Err(LuceneError::IllegalArgument(_))
  ));
  assert!(matches!(
    HnswBitVectorsFormat::with_graph_para(20, MAXIMUM_BEAM_WIDTH + 1),
    Err(LuceneError::IllegalArgument(_))
  ));
  Ok(())
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
