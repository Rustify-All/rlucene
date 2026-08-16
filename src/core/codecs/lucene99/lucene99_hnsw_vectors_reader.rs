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
use crate::core::codecs::CodecUtil;
use crate::core::codecs::hnsw::flat_vectors_reader::FlatVectorsReader;
use crate::core::codecs::hnsw::hnsw_graph_provider::HnswGraphProvider;
use crate::core::codecs::knn_vectors_reader::KnnVectorsReader;
use crate::core::codecs::lucene99::lucene99_hnsw_vectors_format::{
  META_CODEC_NAME, META_EXTENSION, VECTOR_INDEX_CODEC_NAME, VECTOR_INDEX_EXTENSION,
  VERSION_CURRENT, VERSION_START,
};
use crate::core::index::IndexFileNames;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::index::vector_encoding::VectorEncoding;
use crate::core::index::vector_similarity_function::VectorSimilarityFunction;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::knn_collector::KnnCollector;
use crate::core::store::check_sum_index_input::ChecksumIndexInput;
use crate::core::store::directory::Directory;
use crate::core::store::{DataInput, IOContext, IndexInput, ReadAdvice};
use crate::core::util::IOUtils;
use crate::core::util::TryIntoInt;
use crate::core::util::accountable::Accountable;
use crate::core::util::bits::Bits;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::hnsw::hnsw_graph::{
  ArrayNodesIterator, EmptyHnswGraph, HnswGraph, HnswGraphEnum2,
};
use crate::core::util::hnsw::hnsw_graph_searcher::search;
use crate::core::util::hnsw::ordinal_translated_knn_collector::OrdinalTranslatedKnnCollector;
use crate::core::util::hnsw::random_vector_scorer::RandomVectorScorer;
use crate::core::util::long_values::LongValues;
use crate::core::util::packed::direct_monotonic_reader::Meta;
use crate::core::util::packed::direct_monotonic_reader::{DirectMonotonicReader, load_meta};
use crate::core::util::quantization::quantized_vectors_reader::QuantizedVectorsReader;
use crate::core::util::quantization::scalar_quantizer::ScalarQuantizer;
use crate::core::util::ram_usage_estimator::size_of_vec;
use std::collections::HashMap;
use std::mem;
use std::sync::Arc;

/// Reads vectors from the index segments along with index data structures supporting KNN search.
pub struct Lucene99HnswVectorsReader<F, I> {
  flat_vectors_reader: F,
  field_infos: Arc<FieldInfos>,
  fields: HashMap<i32, FieldEntry>,
  vector_index: I,
}
impl<F, I> Lucene99HnswVectorsReader<F, I>
where
  F: FlatVectorsReader,
  I: IndexInput,
{
  pub fn new<D1, D2>(
    state: &SegmentReadState<D1>,
    flat_vectors_reader: F,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self>
  where
    D1: Directory<IndexInput = I>,
  {
    let mut fields = HashMap::new();
    let field_infos = state.field_infos.clone();

    let meta_file_name =
      IndexFileNames::segment_file_name(&segment_info.name, &state.segment_suffix, META_EXTENSION);

    let mut version_meta = -1;

    let mut meta = None;
    let mut vector_index = None;
    let mut meta_close_attempted = false;
    let mut success = false;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      meta = Some(state.directory.open_checksum_input(&meta_file_name)?);

      let body_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        let prior_result =
          std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
            let meta = meta
              .as_mut()
              .ok_or_else(|| LuceneError::illegal_state("HNSW metadata input is missing"))?;
            version_meta = CodecUtil::check_index_header(
              meta,
              META_CODEC_NAME,
              VERSION_START,
              VERSION_CURRENT,
              segment_info.get_id(),
              &state.segment_suffix,
            )?;
            read_fields(meta, field_infos.as_ref(), &mut fields)
          }));
        let meta = meta
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("HNSW metadata input is missing"))?;
        let prior_result = match prior_result {
          Ok(Ok(())) => None,
          prior_result => Some(prior_result),
        };
        CodecUtil::check_footer_with_error(meta, prior_result)?;

        vector_index = Some(Self::open_data_input(
          state,
          version_meta,
          VECTOR_INDEX_EXTENSION,
          VECTOR_INDEX_CODEC_NAME,
          &state.context.with_read_advice_self(ReadAdvice::Random)?,
          segment_info,
        )?);
        success = true;
        Ok(())
      }));
      let meta = meta
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("HNSW metadata input is missing"))?;
      meta_close_attempted = true;
      let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| meta.close()));
      IOUtils::use_or_suppress_caught_result(body_result, close_result)
    }));

    if !success {
      if meta_close_attempted {
        IOUtils::close_while_handling_exception((&flat_vectors_reader, vector_index.as_ref()));
      } else {
        IOUtils::close_while_handling_exception((
          meta.as_ref(),
          &flat_vectors_reader,
          vector_index.as_ref(),
        ));
      }
    }
    unwrap_caught_result!(result)?;
    let vector_index = match vector_index {
      Some(vector_index) => vector_index,
      None => {
        IOUtils::close_while_handling_exception(&flat_vectors_reader);
        return Err(LuceneError::illegal_state(
          "HNSW vector index is missing after successful construction",
        ));
      },
    };

    Ok(Self {
      flat_vectors_reader,
      field_infos,
      fields,
      vector_index,
    })
  }
  fn open_data_input<D1, D2>(
    state: &SegmentReadState<D1>,
    version_meta: i32,
    file_extension: &str,
    codec_name: &str,
    context: &IOContext,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<D1::IndexInput>
  where
    D1: Directory,
  {
    let file_name =
      IndexFileNames::segment_file_name(&segment_info.name, &state.segment_suffix, file_extension);

    let mut input = state.directory.open_input(&file_name, context)?;

    let mut success = false;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      let version_vector_data = CodecUtil::check_index_header(
        &mut input,
        codec_name,
        VERSION_START,
        VERSION_CURRENT,
        segment_info.get_id(),
        &state.segment_suffix,
      )?;

      if version_meta != version_vector_data {
        return Err(LuceneError::corrupt_index(format!(
          "Format versions mismatch: meta={}, {}={}",
          version_meta, codec_name, version_vector_data
        )));
      }

      CodecUtil::retrieve_checksum(&mut input)?;
      success = true;
      Ok(())
    }));
    if !success {
      IOUtils::close_while_handling_exception(&input);
    }
    unwrap_caught_result!(result)?;
    Ok(input)
  }

  fn search<RS, KC, B, S>(
    &self,
    field_entry: &FieldEntry,
    knn_collector: &mut KC,
    accept_docs: Option<B>,
    scorer_supplier: S,
  ) -> Result<()>
  where
    RS: RandomVectorScorer,
    KC: KnnCollector,
    B: Bits,
    S: FnOnce() -> Result<RS>,
  {
    if field_entry.size() == 0 || knn_collector.k() == 0 {
      return Ok(());
    }

    let scorer = scorer_supplier()?;

    let k = knn_collector.k();
    let ord_to_doc = (0..scorer.max_ord())
      .map(|ord| scorer.ord_to_doc(ord))
      .collect::<Result<Vec<_>>>()?;
    let mut collector =
      OrdinalTranslatedKnnCollector::new(knn_collector, |ord| Ok(ord_to_doc[ord]));

    let accepted_ords = scorer.get_accept_ords(accept_docs)?;

    if k < scorer.max_ord() {
      search(
        &scorer,
        &mut collector,
        &mut self.get_graph_from_entry(field_entry)?,
        accepted_ords.as_ref(),
      )?;
    } else {
      for i in 0..scorer.max_ord() {
        let accept = match accepted_ords {
          Some(ref bits) => bits.get(i)?,
          None => true,
        };
        if accept {
          if knn_collector.early_terminated() {
            break;
          }
          knn_collector.inc_visited_count(1);
          knn_collector.collect(scorer.ord_to_doc(i)?, scorer.score(i)?)?;
        }
      }
    }

    Ok(())
  }
  fn get_graph_from_entry(
    &self,
    entry: &FieldEntry,
  ) -> Result<OffHeapHnswGraph<I>> {
    OffHeapHnswGraph::new(entry, &self.vector_index)
  }
  fn get_field_entry(&self, field: &str, expected_encoding: VectorEncoding) -> Result<&FieldEntry> {
    let info = self
      .field_infos
      .field_info_by_name(field)?
      .ok_or_else(|| LuceneError::illegal_argument(format!("field=\"{}\" not found", field)))?;

    let field_entry = self
      .fields
      .get(&info.number)
      .ok_or_else(|| LuceneError::illegal_argument(format!("field=\"{}\" not found", field)))?;

    if field_entry.vector_encoding != expected_encoding {
      return Err(LuceneError::illegal_argument(format!(
        "field=\"{}\" is encoded as: {:?} expected: {:?}",
        field, field_entry.vector_encoding, expected_encoding
      )));
    }

    Ok(field_entry)
  }
}
fn read_fields<M>(
  meta: &mut M,
  field_infos: &FieldInfos,
  fields: &mut HashMap<i32, FieldEntry>,
) -> Result<()>
where
  M: ChecksumIndexInput,
{
  let mut field_number = meta.read_int()?;
  while field_number != -1 {
    let info = field_infos
      .field_info_by_number(field_number)?
      .ok_or_else(|| {
        LuceneError::corrupt_index(format!("Invalid field number: {}", field_number))
      })?;

    let field_entry = read_field(meta, info.as_ref())?;
    validate_field_entry(info.as_ref(), &field_entry)?;

    fields.insert(info.number, field_entry);

    field_number = meta.read_int()?;
  }

  Ok(())
}
fn validate_field_entry(info: &FieldInfo, field_entry: &FieldEntry) -> Result<()> {
  let dimension = info.get_vector_dimension();
  if dimension != field_entry.dimension {
    return Err(LuceneError::illegal_state(format!(
      "Inconsistent vector dimension for field=\"{}\"; {} != {}",
      info.name, dimension, field_entry.dimension
    )));
  }
  Ok(())
}

impl<F, I> Accountable for Lucene99HnswVectorsReader<F, I>
where
  F: Accountable,
{
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(
      self
        .fields
        .ram_bytes_used()?
        .saturating_add(self.flat_vectors_reader.ram_bytes_used()?),
    )
  }
}

impl<F, I> CloseableRef for Lucene99HnswVectorsReader<F, I>
where
  F: CloseableRef,
  I: CloseableRef,
{
  fn close(&self) -> Result<()> {
    IOUtils::close_refs_tuple((Some(&self.flat_vectors_reader), Some(&self.vector_index)))
  }
}

impl<F, I> QuantizedVectorsReader for Lucene99HnswVectorsReader<F, I>
where
  F: FlatVectorsReader + QuantizedVectorsReader,
  I: IndexInput,
{
  type QuantizedByteVectorValues = <F as QuantizedVectorsReader>::QuantizedByteVectorValues;

  fn get_quantized_vector_values(&self, field: &str) -> Result<Self::QuantizedByteVectorValues> {
    self.flat_vectors_reader.get_quantized_vector_values(field)
  }

  fn get_quantization_state(&self, field: &str) -> Result<Option<ScalarQuantizer>> {
    KnnVectorsReader::get_quantization_state(&self.flat_vectors_reader, field)
  }
}
impl<F, I> HnswGraphProvider for Lucene99HnswVectorsReader<F, I>
where
  F: FlatVectorsReader,
  I: IndexInput,
{
  type HnswGraph = HnswGraphEnum2<Box<OffHeapHnswGraph<I>>, EmptyHnswGraph>;

  fn is_hnsw_graph_provider(&self, _field: &str) -> bool {
    true
  }

  fn get_graph(&self, field: &str) -> Result<Self::HnswGraph> {
    let info = self
      .field_infos
      .field_info_by_name(field)?
      .ok_or_else(|| LuceneError::illegal_argument(format!("field=\"{}\" not found", field)))?;

    let entry = self
      .fields
      .get(&info.number)
      .ok_or_else(|| LuceneError::illegal_argument(format!("field=\"{}\" not found", field)))?;

    if entry.vector_index_length > 0 {
      Ok(HnswGraphEnum2::A(Box::new(
        self.get_graph_from_entry(entry)?,
      )))
    } else {
      Ok(HnswGraphEnum2::B(EmptyHnswGraph))
    }
  }
}
impl<F, I> KnnVectorsReader for Lucene99HnswVectorsReader<F, I>
where
  F: FlatVectorsReader,
  I: IndexInput,
{
  fn check_integrity(&self) -> Result<()> {
    self.flat_vectors_reader.check_integrity()?;
    CodecUtil::checksum_entire_file(&self.vector_index)?;
    Ok(())
  }

  type FloatVectorValues = <F as KnnVectorsReader>::FloatVectorValues;

  fn get_float_vector_values(&self, field: &str) -> Result<Self::FloatVectorValues> {
    self.flat_vectors_reader.get_float_vector_values(field)
  }

  type ByteVectorValues = <F as KnnVectorsReader>::ByteVectorValues;

  fn get_byte_vector_values(&self, field: &str) -> Result<Self::ByteVectorValues> {
    self.flat_vectors_reader.get_byte_vector_values(field)
  }

  fn get_quantization_state(&self, field: &str) -> Result<Option<ScalarQuantizer>> {
    self.flat_vectors_reader.get_quantization_state(field)
  }

  fn search_f32<B, K>(
    &self,
    field: &str,
    target: Vec<f32>,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
  {
    let field_entry = self.get_field_entry(field, VectorEncoding::FLOAT32(4))?;
    self.search(field_entry, knn_collector, accept_docs, || {
      self
        .flat_vectors_reader
        .get_random_vector_scorer_f32(field, target)
    })
  }

  fn search_u8<B, K>(
    &self,
    field: &str,
    target: Vec<u8>,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
  {
    let field_entry = self.get_field_entry(field, VectorEncoding::BYTE(1))?;
    self.search(field_entry, knn_collector, accept_docs, || {
      self
        .flat_vectors_reader
        .get_random_vector_scorer_u8(field, target)
    })
  }

  fn finish_merge(&self) -> Result<()> {
    self.flat_vectors_reader.finish_merge()
  }
}

// List of vector similarity functions. This list is defined here, in order
// to avoid an undesirable dependency on the declaration and order of values
// in VectorSimilarityFunction. The list values and order must be identical
// to `Lucene94FieldInfosFormat::SIMILARITY_FUNCTIONS`.
pub const SIMILARITY_FUNCTIONS: &[VectorSimilarityFunction] = &[
  VectorSimilarityFunction::Euclidean,
  VectorSimilarityFunction::DotProduct,
  VectorSimilarityFunction::Cosine,
  VectorSimilarityFunction::MaximumInnerProduct,
];
pub fn read_similarity_function<I>(input: &mut I) -> Result<VectorSimilarityFunction>
where
  I: DataInput,
{
  let i = input.read_int()?;
  if i < 0 || (i as usize) >= SIMILARITY_FUNCTIONS.len() {
    return Err(LuceneError::illegal_argument(format!(
      "invalid distance function: {}",
      i
    )));
  }
  Ok(SIMILARITY_FUNCTIONS[i as usize])
}

pub fn read_vector_encoding<I>(input: &mut I) -> Result<VectorEncoding>
where
  I: DataInput,
{
  let encoding_id = input.read_int()?;
  let values = VectorEncoding::values();
  if encoding_id < 0 || (encoding_id as usize) >= values.len() {
    return Err(LuceneError::corrupt_index(format!(
      "Invalid vector encoding id: {}",
      encoding_id
    )));
  }
  Ok(values[encoding_id as usize])
}
fn read_field<I>(input: &mut I, info: &FieldInfo) -> Result<FieldEntry>
where
  I: IndexInput,
{
  let vector_encoding = read_vector_encoding(input)?;
  let similarity_function = read_similarity_function(input)?;

  if similarity_function != *info.get_vector_similarity_function() {
    return Err(LuceneError::illegal_state(format!(
      "Inconsistent vector similarity function for field=\"{}\"; {:?} != {:?}",
      info.name,
      similarity_function,
      info.get_vector_similarity_function()
    )));
  }

  FieldEntry::create(
    input,
    vector_encoding,
    *info.get_vector_similarity_function(),
  )
}

pub struct FieldEntry {
  similarity_function: VectorSimilarityFunction,
  vector_encoding: VectorEncoding,
  vector_index_offset: usize,
  vector_index_length: usize,
  m: usize,
  num_levels: usize,
  dimension: i32,
  size: usize,
  nodes_by_level: Arc<Vec<Arc<Vec<usize>>>>,
  // for each level the start offsets in vectorIndex file from where to read neighbours
  offsets_meta: Option<Meta>,
  offsets_offset: usize,
  offsets_block_shift: i32,
  offsets_length: usize,
}

impl FieldEntry {
  pub fn create<I>(
    input: &mut I,
    vector_encoding: VectorEncoding,
    similarity_function: VectorSimilarityFunction,
  ) -> Result<Self>
  where
    I: IndexInput,
  {
    let vector_index_offset = input.read_vlong()?.try_convert()?;
    let vector_index_length = input.read_vlong()?.try_convert()?;
    let dimension = input.read_vint()?;
    let size = input.read_int()?.try_convert()?;

    let m = input.read_vint()?.try_convert()?;
    let num_levels = input.read_vint()?.try_convert()?;

    let mut nodes_by_level = Vec::with_capacity(num_levels);

    let mut number_of_offsets: i64 = 0;

    for level in 0..num_levels {
      if level > 0 {
        let num_nodes_on_level = input.read_vint()?.try_convert()?;
        number_of_offsets += num_nodes_on_level as i64;

        let mut level_nodes = vec![0usize; num_nodes_on_level];
        if num_nodes_on_level > 0 {
          level_nodes[0] = input.read_vint()?.try_convert()?;
          for i in 1..num_nodes_on_level {
            level_nodes[i] = level_nodes[i - 1] + input.read_vint()?.try_convert()?;
          }
        }
        nodes_by_level.push(Arc::new(level_nodes));
      } else {
        number_of_offsets += size as i64;
        nodes_by_level.push(Arc::new(Vec::new()));
      }
    }

    let (offsets_offset, offsets_block_shift, offsets_meta, offsets_length) =
      if number_of_offsets > 0 {
        let offsets_offset = input.read_long()?.try_convert()?;
        let offsets_block_shift = input.read_vint()?;
        let offsets_meta = Some(load_meta(input, number_of_offsets, offsets_block_shift)?);
        let offsets_length = input.read_long()?.try_convert()?;
        (
          offsets_offset,
          offsets_block_shift,
          offsets_meta,
          offsets_length,
        )
      } else {
        (0, 0, None, 0)
      };
    let nodes_by_level = Arc::new(nodes_by_level);
    Ok(Self {
      similarity_function,
      vector_encoding,
      vector_index_offset,
      vector_index_length,
      m,
      num_levels,
      dimension,
      size,
      nodes_by_level,
      offsets_meta,
      offsets_offset,
      offsets_block_shift,
      offsets_length,
    })
  }

  pub fn size(&self) -> usize {
    self.size
  }
}

impl Accountable for FieldEntry {
  fn ram_bytes_used(&self) -> Result<i64> {
    let mut size = (mem::size_of_val(self.nodes_by_level.as_ref()) as i64)
      .saturating_add(size_of_vec(self.nodes_by_level.as_ref()));
    for nodes in self.nodes_by_level.iter() {
      size = size
        .saturating_add(mem::size_of_val(nodes.as_ref()) as i64)
        .saturating_add(size_of_vec(nodes.as_ref()));
    }
    if let Some(offsets_meta) = self.offsets_meta.as_ref() {
      size = size.saturating_add(offsets_meta.ram_bytes_used()?);
    }
    Ok(size)
  }
}

pub struct OffHeapHnswGraph<I>
where
  I: IndexInput,
{
  data_in: I::IndexInput,
  nodes_by_level: Arc<Vec<Arc<Vec<usize>>>>,
  num_levels: usize,
  entry_node: usize,
  size: usize,
  arc_count: usize,
  arc_up_to: usize,
  arc: usize,
  graph_level_node_offsets: DirectMonotonicReader<I::RandomAccessSlice>,
  graph_level_node_index_offsets: Vec<usize>,
  // Allocated to be M*2 to track the current neighbors being explored
  current_neighbors_buffer: Vec<usize>,
}
impl<I> OffHeapHnswGraph<I>
where
  I: IndexInput,
{
  pub fn new(entry: &FieldEntry, vector_index: &I) -> Result<Self> {
    let data_in = vector_index.slice(
      "graph-data",
      entry.vector_index_offset,
      entry.vector_index_length,
    )?;

    let nodes_by_level = entry.nodes_by_level.clone();
    let num_levels = entry.num_levels;
    let entry_node = if num_levels > 1 {
      nodes_by_level[num_levels - 1][0]
    } else {
      0
    };

    let size = entry.size();

    let addresses_data =
      vector_index.random_access_slice(entry.offsets_offset, entry.offsets_length)?;

    let graph_level_node_offsets = DirectMonotonicReader::get_instance(
      entry
        .offsets_meta
        .as_ref()
        .ok_or_else(|| LuceneError::illegal_state("meta is None"))?,
      addresses_data,
    )?;

    let current_neighbors_buffer = vec![0usize; entry.m * 2];

    let mut graph_level_node_index_offsets = vec![0usize; num_levels];
    graph_level_node_index_offsets[0] = 0;

    for i in 1..num_levels {
      let node_count = if nodes_by_level[i - 1].is_empty() {
        size
      } else {
        nodes_by_level[i - 1].len()
      };
      graph_level_node_index_offsets[i] = graph_level_node_index_offsets[i - 1] + node_count;
    }

    Ok(Self {
      data_in,
      nodes_by_level,
      num_levels,
      entry_node,
      size,
      arc_count: 0,
      arc_up_to: 0,
      arc: 0,
      graph_level_node_offsets,
      graph_level_node_index_offsets,
      current_neighbors_buffer,
    })
  }
}
impl<I> HnswGraph for OffHeapHnswGraph<I>
where
  I: IndexInput,
{
  fn seek(&mut self, level: usize, target_ord: usize) -> Result<()> {
    let target_index = if level == 0 {
      target_ord
    } else {
      let nodes = &self.nodes_by_level[level];
      match nodes.binary_search(&target_ord) {
        Ok(idx) => idx,
        Err(_) => {
          debug_assert!(false, "target_ord not found in level");
          return Err(LuceneError::illegal_state("target_ord not found"));
        },
      }
    };

    let offset = self
      .graph_level_node_offsets
      .get_mut(target_index + self.graph_level_node_index_offsets[level])?;

    self.data_in.seek(offset as usize)?;

    self.arc_count = self.data_in.read_vint()?.try_convert()?;

    debug_assert!(
      self.arc_count <= self.current_neighbors_buffer.len(),
      "too many neighbors: {}",
      self.arc_count
    );

    if self.arc_count > 0 {
      self.current_neighbors_buffer[0] = self.data_in.read_vint()?.try_convert()?;
      for i in 1..self.arc_count {
        let delta = self.data_in.read_vint()?.try_convert()?;
        self.current_neighbors_buffer[i] = self.current_neighbors_buffer[i - 1] + delta;
      }
    }
    self.arc_up_to = 0;

    Ok(())
  }

  fn size(&self) -> usize {
    self.size
  }

  fn next_neighbor(&mut self) -> Result<usize> {
    if self.arc_up_to >= self.arc_count {
      return Ok(NO_MORE_DOCS as usize);
    }
    self.arc = self.current_neighbors_buffer[self.arc_up_to];
    self.arc_up_to += 1;
    Ok(self.arc)
  }

  fn num_levels(&self) -> Result<usize> {
    Ok(self.num_levels)
  }

  fn entry_node(&self) -> Result<Option<usize>> {
    Ok(Some(self.entry_node))
  }

  type NodeIterator = ArrayNodesIterator;

  fn get_nodes_on_level(&mut self, level: usize) -> Result<Self::NodeIterator> {
    if level == 0 {
      Ok(ArrayNodesIterator::from_size(self.size()))
    } else {
      let nodes = self.nodes_by_level[level].clone();
      let len = nodes.len();
      Ok(ArrayNodesIterator::from_nodes(Some(nodes), len))
    }
  }
}
