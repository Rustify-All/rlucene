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
use crate::core::codecs::KnnVectorsFormatsReader;
use crate::core::codecs::codec_formats::CodecKnnVectorsReaderInner;
use crate::core::codecs::hnsw::default_flat_vector_scorer::DefaultFlatVectorScorer;
use crate::core::codecs::knn_field_vectors_writer::VectorValueEnum;
use crate::core::codecs::lucene95::has_index_slice::HasIndexSlice;
use crate::core::codecs::lucene99::lucene99_hnsw_scalar_quantized_vectors_format::Lucene99HnswScalarQuantizedVectorsFormat;
use crate::core::codecs::lucene99::lucene99_hnsw_vectors_format::{
  DEFAULT_BEAM_WIDTH, DEFAULT_MAX_CONN,
};
use crate::core::codecs::lucene99::lucene99_scalar_quantized_vector_scorer::{
  Lucene99ScalarQuantizedVectorScorer, ScalarQuantizedRandomVectorScorerEnum,
};
use crate::core::codecs::lucene99::off_heap_quantized_byte_vector_values::compress_bytes;
use crate::core::document::document::Document;
use crate::core::document::field::Store;
use crate::core::document::knn_float_vector_field::KnnFloatVectorField;
use crate::core::index::byte_vector_values::ByteVectorValues;
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::directory_reader;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::knn_vector_values::{DenseDocIndexIterator, KnnVectorValues};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::index::vector_encoding::VectorEncoding;
use crate::core::index::vector_similarity_function::VectorSimilarityFunction;
use crate::core::index::{index_writer::IndexWriter, index_writer_config::IndexWriterConfig};
use crate::core::search::dummy::dummy_vector_scorer::DummyVectorScorer;
use crate::core::search::knn_collector::KnnCollector;
use crate::core::search::top_knn_collector::TopKnnCollector;
use crate::core::store::directory::Directory;
use crate::core::store::{DataInput, DataOutput, IOContext, IndexInput, IndexOutput};
use crate::core::util::bits::Bits;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::dummy::dummy_bits::DummyBits;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::hnsw::random_vector_scorer::RandomVectorScorer;
use crate::core::util::quantization::quantized_byte_vector_values::QuantizedByteVectorValues;
use crate::core::util::quantization::quantized_vectors_reader::QuantizedVectorsReader;
use crate::core::util::quantization::scalar_quantizer::ScalarQuantizer;
use crate::core::util::vector_util::VectorUtil;
use crate::test_framework::core::codecs::asserting_codec::AssertingCodec;
use crate::test_framework::core::codecs::asserting_codec::AssertingCodecKnnVectorsReader;
use crate::test_framework::core::util::lucene_test_case::{
  get_only_leaf_reader, new_directory_shared, new_text_field, random,
};
use crate::test_framework::core::util::test_util::TestUtil;
use parking_lot::Mutex;
use rand::{Rng, RngExt};
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

#[allow(dead_code)] // for quick search
struct TestLucene99ScalarQuantizedVectorScorer;

fn get_codec(bits: i32, compress: bool) -> Result<AssertingCodec> {
  Ok(TestUtil::always_knn_vectors_format(
    Lucene99HnswScalarQuantizedVectorsFormat::
      with_graph_para_with_threads_bits_compress_confidence_interval(
        DEFAULT_MAX_CONN,
        DEFAULT_BEAM_WIDTH,
        1,
        bits,
        compress,
        Some(0.0),
      )?,
  ))
}

#[test]
fn test_non_zero_scores() -> Result<()> {
  for bits in [4, 7] {
    for compress in [true, false] {
      vector_non_zero_scoring_test(bits, compress)?;
    }
  }
  Ok(())
}

fn vector_non_zero_scoring_test(bits: i32, compress: bool) -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  // keep vecs `0` so dot product is `0`
  let mut vec1 = vec![0_u8; 32];
  let mut vec2 = vec![0_u8; 32];
  if compress && bits == 4 {
    let mut vec1_compressed = vec![0; 16];
    let mut vec2_compressed = vec![0; 16];
    compress_bytes(&vec1, &mut vec1_compressed)?;
    compress_bytes(&vec2, &mut vec2_compressed)?;
    vec1 = vec1_compressed;
    vec2 = vec2_compressed;
  }
  let file_name = "test_non_zero_scores-32";
  {
    let mut out = dir.create_output(file_name, &IOContext::default_io_context()?)?;
    // large negative offset to override any query score correction and
    // ensure negative values that need to be snapped to `0`
    let negative_offset = (-50_f32).to_le_bytes();
    let mut bytes = Vec::with_capacity(vec1.len() + vec2.len() + 8);
    bytes.extend_from_slice(&vec1);
    bytes.extend_from_slice(&negative_offset);
    bytes.extend_from_slice(&vec2);
    bytes.extend_from_slice(&negative_offset);
    out.write_bytes_with_len(&bytes, bytes.len())?;
    out.close()?;
  }
  let input = Arc::new(Mutex::new(
    dir.open_input(file_name, &IOContext::default_io_context()?)?,
  ));
  {
    let scalar_quantizer = ScalarQuantizer::new(0.1, 0.9, bits as u8)?;
    let values = TestQuantizedByteVectorValues {
      dimension: 32,
      vector_byte_length: if compress && bits == 4 { 16 } else { 32 },
      scalar_quantizer,
      input: Arc::clone(&input),
    };
    let scorer = Lucene99ScalarQuantizedVectorScorer::new(DefaultFlatVectorScorer);
    let query_vector = (0..32).map(|i| i as f32 * 0.1).collect::<Vec<_>>();
    for function in [
      VectorSimilarityFunction::Euclidean,
      VectorSimilarityFunction::DotProduct,
      VectorSimilarityFunction::Cosine,
      VectorSimilarityFunction::MaximumInnerProduct,
    ] {
      let random_scorer =
        scorer.get_random_vector_scorer_f32(function, values.clone(), query_vector.clone())?;
      assert!(random_scorer.score(0)? >= 0.0);
      assert!(random_scorer.score(1)? >= 0.0);
    }
  }
  input.lock().close()?;
  dir.close()
}

#[test]
fn test_scoring_compressed_int4() -> Result<()> {
  vector_scoring_test(4, true)
}

#[test]
fn test_scoring_uncompressed_int4() -> Result<()> {
  vector_scoring_test(4, false)
}

#[test]
fn test_scoring_int7() -> Result<()> {
  vector_scoring_test(7, false)
}

fn vector_scoring_test(bits: i32, compress: bool) -> Result<()> {
  let mut random = random();
  let num_vectors = 10;
  let mut vector_dimensions = random.random_range(4..14);
  if bits == 4 && vector_dimensions % 2 == 1 {
    vector_dimensions += 1;
  }
  let mut stored_vectors = Vec::with_capacity(num_vectors);
  for i in 0..num_vectors {
    let mut vector = (0..vector_dimensions)
      .map(|j| (i + j) as f32)
      .collect::<Vec<_>>();
    VectorUtil::l2normalize(&mut vector)?;
    stored_vectors.push(vector);
  }

  // create lucene directory with codec
  for similarity_function in [
    VectorSimilarityFunction::DotProduct,
    VectorSimilarityFunction::MaximumInnerProduct,
    VectorSimilarityFunction::Euclidean,
  ] {
    let dir = new_directory_shared(&mut random)?;
    index_vectors(
      dir.clone(),
      &stored_vectors,
      similarity_function,
      bits,
      compress,
      &mut random,
    )?;
    {
      let reader = directory_reader::open(dir.clone())?;
      let leaf = get_only_leaf_reader(reader)?;
      let mut vector = (0..vector_dimensions)
        .map(|i| (i + 1) as f32)
        .collect::<Vec<_>>();
      VectorUtil::l2normalize(&mut vector)?;

      let vector_reader = leaf
        .get_vector_reader()?
        .expect("vector reader should exist");
      let fields_reader = match vector_reader.as_ref().as_inner() {
        CodecKnnVectorsReaderInner::Asserting(fields_reader) => fields_reader,
        _ => panic!("reader is not the Asserting codec's per-field KNN reader"),
      };
      let field_reader = fields_reader
        .get_field_reader("field")?
        .expect("field reader should exist");
      let source_reader = match field_reader.as_ref() {
        AssertingCodecKnnVectorsReader::Source(source_reader) => source_reader,
        _ => panic!("field reader is not a source KNN format reader"),
      };
      let quantized_reader = match source_reader {
        KnnVectorsFormatsReader::Lucene99HnswScalarQuantized(reader) => reader,
        _ => panic!("reader is not Lucene99HnswVectorsReader"),
      };
      let quantized_values = quantized_reader.get_quantized_vector_values("field")?;
      let random_scorer =
        get_random_vector_scorer(similarity_function, quantized_values, vector.clone())?;
      let raw_scores = stored_vectors
        .iter()
        .map(|stored_vector| similarity_function.compare_f32(&vector, stored_vector))
        .collect::<Result<Vec<_>>>()?;
      for (i, raw_score) in raw_scores.into_iter().enumerate() {
        assert!((raw_score - random_scorer.score(i)?).abs() <= 0.05);
      }
      leaf.close()?;
    }
    dir.close()?;
  }
  Ok(())
}

fn get_random_vector_scorer<V>(
  function: VectorSimilarityFunction,
  values: V,
  vector: Vec<f32>,
) -> Result<ScalarQuantizedRandomVectorScorerEnum<V>>
where
  V: QuantizedByteVectorValues,
{
  Lucene99ScalarQuantizedVectorScorer::new(DefaultFlatVectorScorer)
    .get_random_vector_scorer_f32(function, values, vector)
}

fn index_vectors<D, R>(
  dir: Arc<D>,
  vectors: &[Vec<f32>],
  function: VectorSimilarityFunction,
  bits: i32,
  compress: bool,
  random: &mut R,
) -> Result<()>
where
  D: Directory + 'static,
  R: Rng + ?Sized,
{
  let mut config = IndexWriterConfig::new()?;
  config.set_codec(get_codec(bits, compress)?);
  let writer = IndexWriter::new(dir, config)?;
  for vector in vectors {
    if random.random_bool(0.5) {
      // index a document without a vector
      writer.add_document(Document::new())?;
    }
    writer.add_document(Document::new())?;
    let mut doc = Document::new();
    doc.add(KnnFloatVectorField::with_similarity_function(
      "field",
      vector.clone(),
      function,
    )?);
    writer.add_document(doc)?;
  }
  writer.commit()?;
  writer.force_merge(1)?;
  writer.close()
}

#[test]
fn test_single_vector_per_segment_cosine() -> Result<()> {
  test_single_vector_per_segment(VectorSimilarityFunction::Cosine)
}

#[test]
fn test_single_vector_per_segment_dot() -> Result<()> {
  test_single_vector_per_segment(VectorSimilarityFunction::DotProduct)
}

#[test]
fn test_single_vector_per_segment_euclidean() -> Result<()> {
  test_single_vector_per_segment(VectorSimilarityFunction::Euclidean)
}

#[test]
fn test_single_vector_per_segment_mip() -> Result<()> {
  test_single_vector_per_segment(VectorSimilarityFunction::MaximumInnerProduct)
}

fn test_single_vector_per_segment(similarity_function: VectorSimilarityFunction) -> Result<()> {
  let mut random = random();
  let codec = get_codec(7, false)?;
  let dir = new_directory_shared(&mut random)?;
  let mut config = IndexWriterConfig::new()?;
  config.set_codec(codec);
  let writer = IndexWriter::new(dir.clone(), config)?;
  let mut field_to_type = HashMap::new();

  let mut doc2 = Document::new();
  doc2.add(KnnFloatVectorField::with_similarity_function(
    "field",
    vec![0.8, 0.6],
    similarity_function,
  )?);
  doc2.add(new_text_field(
    &mut random,
    "id",
    "A",
    Store::Yes,
    &mut field_to_type,
  )?);
  writer.add_document(doc2)?;
  writer.commit()?;

  let mut doc1 = Document::new();
  doc1.add(KnnFloatVectorField::with_similarity_function(
    "field",
    vec![0.6, 0.8],
    similarity_function,
  )?);
  doc1.add(new_text_field(
    &mut random,
    "id",
    "B",
    Store::Yes,
    &mut field_to_type,
  )?);
  writer.add_document(doc1)?;
  writer.commit()?;

  let mut doc3 = Document::new();
  doc3.add(KnnFloatVectorField::with_similarity_function(
    "field",
    vec![-0.6, -0.8],
    similarity_function,
  )?);
  doc3.add(new_text_field(
    &mut random,
    "id",
    "C",
    Store::Yes,
    &mut field_to_type,
  )?);
  writer.add_document(doc3)?;
  writer.commit()?;
  writer.force_merge(1)?;
  writer.close()?;

  {
    let reader = directory_reader::open(dir.clone())?;
    let leaf = get_only_leaf_reader(reader)?;
    let mut stored_fields = IndexReader::stored_fields(&leaf)?;
    let mut collector = TopKnnCollector::new(3, 100)?;
    LeafReader::search_nearest_vectors_f32(
      &leaf,
      "field",
      vec![0.6, 0.8],
      &mut collector,
      Option::<DummyBits>::None,
    )?;
    let hits = collector.top_docs()?;
    assert_eq!(3, hits.score_docs.len());
    assert_eq!(
      "B",
      stored_fields
        .document(hits.score_docs[0].doc)?
        .get("id")?
        .expect("stored id")
        .as_ref()
    );
    assert_eq!(
      "A",
      stored_fields
        .document(hits.score_docs[1].doc)?
        .get("id")?
        .expect("stored id")
        .as_ref()
    );
    assert_eq!(
      "C",
      stored_fields
        .document(hits.score_docs[2].doc)?
        .get("id")?
        .expect("stored id")
        .as_ref()
    );
    leaf.close()?;
  }
  dir.close()
}

struct TestQuantizedByteVectorValues<I> {
  dimension: usize,
  vector_byte_length: usize,
  scalar_quantizer: ScalarQuantizer,
  input: Arc<Mutex<I>>,
}

impl<I> Clone for TestQuantizedByteVectorValues<I>
where
  I: IndexInput,
{
  fn clone(&self) -> Self {
    Self {
      dimension: self.dimension,
      vector_byte_length: self.vector_byte_length,
      scalar_quantizer: self.scalar_quantizer.clone(),
      input: Arc::clone(&self.input),
    }
  }
}

impl<I> HasIndexSlice for TestQuantizedByteVectorValues<I>
where
  I: IndexInput,
{
  fn seek(&self, pos: usize) -> Result<()> {
    self.input.lock().seek(pos)
  }

  fn read_bytes(&self, bytes: &mut [u8], offset: usize, len: usize) -> Result<()> {
    self.input.lock().read_bytes(bytes, offset, len)
  }
}

impl<I> KnnVectorValues for TestQuantizedByteVectorValues<I>
where
  I: IndexInput,
{
  fn dimension(&self) -> usize {
    self.dimension
  }

  fn size(&self) -> usize {
    2
  }

  type KnnVectorValues = Self;

  fn copy(&self) -> Result<Self::KnnVectorValues> {
    Ok(self.clone())
  }

  fn get_vector_byte_length(&self) -> usize {
    self.vector_byte_length
  }

  fn get_encoding(&self) -> VectorEncoding {
    VectorEncoding::BYTE(1)
  }

  type Bits<'a, B>
    = DummyBits
  where
    B: Bits,
    Self: 'a;

  fn get_accept_ords<'a, B>(&'a self, _accept_docs: Option<B>) -> Option<Self::Bits<'a, B>>
  where
    B: Bits,
  {
    None
  }

  type DocIndexIterator = DenseDocIndexIterator;
}

impl<I> ByteVectorValues for TestQuantizedByteVectorValues<I>
where
  I: IndexInput,
{
  fn vector_value(&self, _ord: usize) -> Result<Cow<'_, VectorValueEnum>> {
    Ok(Cow::Owned(VectorValueEnum::Byte(vec![0; self.dimension])))
  }

  type ByteVectorValues = Self;

  fn byte_copy(&self) -> Result<Option<Self::ByteVectorValues>> {
    Ok(Some(self.clone()))
  }

  type VectorScorer = DummyVectorScorer;
}

impl<I> QuantizedByteVectorValues for TestQuantizedByteVectorValues<I>
where
  I: IndexInput,
{
  fn get_scalar_quantizer(&self) -> Result<ScalarQuantizer> {
    Ok(self.scalar_quantizer.clone())
  }

  fn get_score_correction_constant(&self, _ord: usize) -> Result<f32> {
    Ok(-50.0)
  }

  type QuantizedVectorScorer = DummyVectorScorer;
  type QuantizedByteVectorValues = Self;

  fn copy(&self) -> Result<Self::QuantizedByteVectorValues> {
    Ok(self.clone())
  }
}
