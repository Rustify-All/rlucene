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
use crate::core::codecs::hnsw::flat_field_vectors_writer::FlatFieldVectorsWriter;
use crate::core::codecs::hnsw::flat_vectors_scorer::FlatVectorsScorer;
use crate::core::codecs::hnsw::flat_vectors_writer::{FlatVectorsWriter, FlatVectorsWriterSs};
use crate::core::codecs::knn_field_vectors_writer::KnnFieldVectorsWriter;
use crate::core::codecs::knn_field_vectors_writer::VectorInput;
use crate::core::codecs::knn_vectors_writer::{
  KnnVectorsWriter, has_vector_values, map_old_ord_to_new_ord, merge_byte_vector_values,
  merge_float_vector_values,
};
use crate::core::codecs::lucene99::lucene99_hnsw_vectors_format::{
  DIRECT_MONOTONIC_BLOCK_SHIFT, META_CODEC_NAME, META_EXTENSION, VECTOR_INDEX_CODEC_NAME,
  VECTOR_INDEX_EXTENSION, VERSION_CURRENT,
};
use crate::core::codecs::lucene99::lucene99_hnsw_vectors_reader::SIMILARITY_FUNCTIONS;
use crate::core::index::byte_vector_values::{ByteVectorValuesImpl, from_bytes};
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::docs_with_field_set::DocsWithFieldSet;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::float_vector_values::{FloatVectorValuesImpl, from_floats};
use crate::core::index::merge_state::MergeState;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::sorter::DocMap;
use crate::core::index::vector_encoding::VectorEncoding;
use crate::core::index::vector_similarity_function::{
  CosineSimilarity, DotProductSimilarity, EuclideanSimilarity, MaximumInnerProductSimilarity,
  VectorSimilarity, VectorSimilarityFunction,
};
use crate::core::search::task_executor::TaskExecutor;
use crate::core::store::directory::Directory;
use crate::core::store::{DataOutput, IndexOutput};
use crate::core::util::TryIntoInt;
use crate::core::util::accountable::Accountable;
use crate::core::util::close::Closeable;
use crate::core::util::concurrent_hnsw_merger::ConcurrentHnswMerger;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::hnsw::closeable_random_vector_scorer_supplier::{
  CloseableRandomVectorScorerSupplier, CloseableRandomVectorScorerSupplierEnum2,
};
use crate::core::util::hnsw::hnsw_builder::HnswBuilder;
use crate::core::util::hnsw::hnsw_graph::{
  ArrayNodesIterator, HnswGraph, NodesIterator, NodesIteratorEnum2,
};
use crate::core::util::hnsw::hnsw_graph_builder::{DefaultHnswGraphBuilder, create, rand_seed};
use crate::core::util::hnsw::hnsw_graph_merger::HnswGraphMerger;
use crate::core::util::hnsw::neighbor_array::NeighborArray;
use crate::core::util::hnsw::on_heap_hnsw_graph::OnHeapHnswGraph;
use crate::core::util::hnsw::random_vector_scorer_supplier::RandomVectorScorerSupplier;
use crate::core::util::incremental_hnsw_graph_merger::{
  HnswGraphMergerHook, IncrementalHnswGraphMerger,
};
use crate::core::util::info_stream::InfoStreamMT;
use crate::core::util::io_utils::IOUtils;
use crate::core::util::packed::direct_monotonic_writer::DirectMonotonicWriter;
use crate::core::util::ram_usage_estimator::size_of_vec;
use std::sync::Arc;

/// Writes vector values and knn graphs to index segments.
pub struct Lucene99HnswVectorsWriter<F>
where
  F: FlatVectorsWriter,
{
  meta: F::IndexOutput,
  vector_index: F::IndexOutput,
  m: usize,
  beam_width: usize,
  pub(crate) flat_vector_writer: F,
  num_merge_workers: usize,
  merge_exec: Option<Arc<TaskExecutor>>,
  finished: bool,
  info_stream: InfoStreamMT,
  fields: Vec<EncodedFieldWriter<F::FlatVectorsScorer>>,
  merge_offsets: Vec<i32>,
}
pub type DefaultRandomVectorScorerSupplier<F> =
  FlatVectorsWriterSs<F, ByteVectorValuesImpl, FloatVectorValuesImpl>;

impl<F> Lucene99HnswVectorsWriter<F>
where
  F: FlatVectorsWriter,
{
  pub fn new<D1, D2>(
    state: &SegmentWriteState<D1>,
    m: usize,
    beam_width: usize,
    mut flat_vector_writer: F,
    num_merge_workers: usize,
    merge_exec: Option<Arc<TaskExecutor>>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self>
  where
    D1: Directory<IndexOutput = F::IndexOutput>,
  {
    let mut meta = None;
    let mut vector_index = None;
    let mut success = false;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      meta = Some(state.create_segment_output(&segment_info.name, META_EXTENSION)?);
      vector_index = Some(state.create_segment_output(&segment_info.name, VECTOR_INDEX_EXTENSION)?);
      CodecUtil::write_index_header(
        meta
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("meta output is missing"))?,
        META_CODEC_NAME,
        VERSION_CURRENT,
        segment_info.get_id(),
        &state.segment_suffix,
      )?;

      CodecUtil::write_index_header(
        vector_index
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("vector index output is missing"))?,
        VECTOR_INDEX_CODEC_NAME,
        VERSION_CURRENT,
        segment_info.get_id(),
        &state.segment_suffix,
      )?;
      success = true;
      Ok(())
    }));

    if !success {
      IOUtils::close_while_handling_exception((
        meta.as_mut(),
        vector_index.as_mut(),
        &mut flat_vector_writer,
      ));
    }
    unwrap_caught_result!(result)?;
    let (meta, vector_index) = match (meta, vector_index) {
      (Some(meta), Some(vector_index)) => (meta, vector_index),
      (mut meta, mut vector_index) => {
        IOUtils::close_while_handling_exception((
          meta.as_mut(),
          vector_index.as_mut(),
          &mut flat_vector_writer,
        ));
        return Err(LuceneError::illegal_state(
          "HNSW writer outputs are missing after successful construction",
        ));
      },
    };

    Ok(Self {
      meta,
      vector_index,
      m,
      beam_width,
      flat_vector_writer,
      num_merge_workers,
      merge_exec,
      finished: false,
      info_stream: state.info_stream.clone(),
      fields: Vec::new(),
      merge_offsets: Vec::new(),
    })
  }
  fn write_field(&mut self, field_data: usize, offsets: &mut Vec<i32>) -> Result<()> {
    let field_data = self
      .fields
      .get_mut(field_data)
      .ok_or_else(|| LuceneError::illegal_state("HNSW field data is missing"))?;
    let vector_index_offset = self.vector_index.get_file_pointer()?;

    let flat_field_vectors_writers = self.flat_vector_writer.get_fields_mut();
    let cardinality = field_data
      .get_docs_with_field_set(flat_field_vectors_writers)?
      .cardinality();
    let (field_info, mut graph) =
      field_data.get_graph_and_field_info(flat_field_vectors_writers)?;
    Self::write_graph(&mut self.vector_index, graph.as_deref_mut(), offsets)?;

    let vector_index_length = self.vector_index.get_file_pointer()? - vector_index_offset;
    Self::write_meta(
      &mut self.vector_index,
      &mut self.meta,
      self.m,
      field_info,
      vector_index_offset.try_convert()?,
      vector_index_length.try_convert()?,
      cardinality,
      graph,
      offsets,
    )?;

    Ok(())
  }
  fn write_sorting_field<DM>(
    &mut self,
    field_data_idx: usize,
    sort_map: &DM,
    offsets: &mut Vec<i32>,
    ordinal_maps: &mut smallvec::SmallVec<[usize; 16]>,
  ) -> Result<()>
  where
    DM: DocMap,
  {
    let flat_field_vectors_writers = self.flat_vector_writer.get_fields_mut();
    let field_data = self
      .fields
      .get_mut(field_data_idx)
      .ok_or_else(|| LuceneError::illegal_state("HNSW field data is missing"))?;
    let cardinality = field_data
      .get_docs_with_field_set(flat_field_vectors_writers)?
      .cardinality() as usize;

    ordinal_maps.clear();
    ordinal_maps.reserve_exact(cardinality * 2);
    ordinal_maps.resize(cardinality * 2, 0);
    let (old_ord_map, ord_map) = ordinal_maps.split_at_mut(cardinality);

    map_old_ord_to_new_ord(
      field_data.get_docs_with_field_set(flat_field_vectors_writers)?,
      sort_map,
      Some(&mut *old_ord_map),
      Some(&mut *ord_map),
      None,
    )?;

    let vector_index_offset = self.vector_index.get_file_pointer()?;

    let count = field_data
      .get_docs_with_field_set(flat_field_vectors_writers)?
      .cardinality();
    let (field_info, graph) = field_data.get_graph_and_field_info(flat_field_vectors_writers)?;

    let mut mock_graph = Self::reconstruct_and_write_graph(
      &mut self.vector_index,
      graph,
      ord_map,
      old_ord_map,
      offsets,
    )?;

    let vector_index_length = self.vector_index.get_file_pointer()? - vector_index_offset;

    let mut graph_view = mock_graph
      .as_mut()
      .map(|(graph, upper_level_nodes)| HnswGraphImpl::new(graph, upper_level_nodes));

    Self::write_meta(
      &mut self.vector_index,
      &mut self.meta,
      self.m,
      field_info,
      vector_index_offset.try_convert()?,
      vector_index_length.try_convert()?,
      count,
      graph_view.as_mut(),
      offsets,
    )?;

    Ok(())
  }
  /// Reconstructs the graph given the old and new node ids.
  ///
  /// Additionally, the graph node connections are written to the vectorIndex.
  ///
  /// # Arguments
  /// * `graph` - The current on heap graph
  /// * `new_to_old_map` - the new node ids indexed to the old node ids
  /// * `old_to_new_map` - the old node ids indexed to the new node ids
  /// * `level_node_offsets` - where to place the new offsets for the nodes in the vector index.
  ///
  /// # Returns
  /// The graph
  ///
  /// # Errors
  /// if writing to vectorIndex fails
  fn reconstruct_and_write_graph<'a>(
    vector_index: &mut F::IndexOutput,
    graph: Option<&'a mut OnHeapHnswGraph>,
    new_to_old_map: &[usize],
    old_to_new_map: &[usize],
    level_node_offsets: &mut Vec<i32>,
  ) -> Result<Option<ReconstructedGraph<'a>>> {
    let Some(graph) = graph else {
      return Ok(None);
    };
    let num_levels = graph.num_levels()?;
    let mut upper_level_nodes = smallvec::SmallVec::with_capacity(num_levels.saturating_sub(1));

    let max_ord = graph.size();
    let mut nodes_on_level0 = graph.get_nodes_on_level(0)?;
    level_node_offsets.reserve_exact(max_ord + *graph.non_zero_level_size.get_mut());

    while nodes_on_level0.has_next() {
      let node = nodes_on_level0
        .next()
        .ok_or_else(|| LuceneError::illegal_state("Expected more nodes on level 0"))?;
      let neighbors = graph.get_neighbors_mut(0, new_to_old_map[node])?;

      let offset = vector_index.get_file_pointer()?;

      Self::reconstruct_and_write_neighbours(vector_index, neighbors, old_to_new_map, max_ord)?;

      let delta = (vector_index.get_file_pointer()? - offset).try_convert()?;

      level_node_offsets.push(delta);
    }

    for level in 1..num_levels {
      let mut nodes_on_level = graph.get_nodes_on_level(level)?;
      let mut new_nodes = Vec::with_capacity(nodes_on_level.size());

      while nodes_on_level.has_next() {
        let node = nodes_on_level
          .next()
          .ok_or_else(|| LuceneError::illegal_state("Expected more nodes on level"))?;
        new_nodes.push(old_to_new_map[node]);
      }

      new_nodes.sort_unstable();

      for &node in &new_nodes {
        let neighbors = graph.get_neighbors_mut(level, new_to_old_map[node])?;

        let offset = vector_index.get_file_pointer()?;

        Self::reconstruct_and_write_neighbours(vector_index, neighbors, old_to_new_map, max_ord)?;

        let delta = (vector_index.get_file_pointer()? - offset).try_convert()?;

        level_node_offsets.push(delta);
      }
      upper_level_nodes.push(new_nodes);
    }

    Ok(Some((graph, upper_level_nodes)))
  }
  fn reconstruct_and_write_neighbours(
    vector_index: &mut F::IndexOutput,
    neighbors: &mut NeighborArray<true>,
    old_to_new_map: &[usize],
    max_ord: usize,
  ) -> Result<()> {
    let size = neighbors.size();
    vector_index.write_vint(size as i32)?;

    let nnodes = neighbors.nodes_mut();

    for node in nnodes.iter_mut().take(size) {
      *node = old_to_new_map[*node as usize].try_convert()?;
    }

    nnodes[..size].sort_unstable();

    Self::write_neighbor_deltas(vector_index, &nnodes[..size], max_ord)?;

    Ok(())
  }

  fn write_neighbor_deltas(out: &mut F::IndexOutput, sorted: &[u32], max_ord: usize) -> Result<()> {
    debug_assert!(sorted.iter().skip(1).all(|&node| (node as usize) < max_ord));
    let mut previous = 0;
    for &node in sorted {
      out.write_vint((node - previous) as i32)?;
      previous = node;
    }
    Ok(())
  }

  /// Writes `graph` in compressed form and records non-cumulative node offsets.
  ///
  /// # Errors
  ///
  /// Returns an I/O error if writing to `vector_index` fails.
  fn write_graph(
    vector_index: &mut F::IndexOutput,
    graph: Option<&mut OnHeapHnswGraph>,
    offsets: &mut Vec<i32>,
  ) -> Result<()> {
    let Some(graph) = graph else {
      return Ok(());
    };

    let count_on_level0 = graph.size();
    let num_levels = graph.num_levels()?;
    offsets.reserve_exact(count_on_level0 + *graph.non_zero_level_size.get_mut());

    for level in 0..num_levels {
      let nodes = graph.get_nodes_on_level(level)?;
      // This private writer only accepts OnHeapHnswGraph. Level 0 is 0..size,
      // and generate_level_to_nodes builds upper levels in ascending node order.
      // Writing neighbors does not change these completed level directories.
      for node in nodes {
        let neighbors = graph.get_neighbors_mut(level, node)?;
        let size = neighbors.size();

        let offset_start = vector_index.get_file_pointer()?;

        vector_index.write_vint(size as i32)?;

        // This is the final graph write: neighbors are discarded after encoding.
        let nnodes = &mut neighbors.nodes_mut()[..size];
        nnodes.sort_unstable();

        Self::write_neighbor_deltas(vector_index, nnodes, count_on_level0)?;

        let offset = (vector_index.get_file_pointer()? - offset_start).try_convert()?;

        offsets.push(offset);
      }
    }

    Ok(())
  }
  #[allow(clippy::too_many_arguments)]
  fn write_meta<H>(
    vector_index: &mut F::IndexOutput,
    meta: &mut F::IndexOutput,
    m: usize,
    field: &FieldInfo,
    vector_index_offset: i64,
    vector_index_length: i64,
    count: i32,
    graph: Option<&mut H>,
    graph_level_node_offsets: &[i32],
  ) -> Result<()>
  where
    H: HnswGraph,
  {
    meta.write_int(field.number)?;
    meta.write_int(field.get_vector_encoding().ordinal())?;
    meta.write_int(dist_func_to_ord(field.get_vector_similarity_function())? as i32)?;
    meta.write_vlong(vector_index_offset)?;
    meta.write_vlong(vector_index_length)?;
    meta.write_vint(field.get_vector_dimension())?;
    meta.write_int(count)?;
    meta.write_vint(m as i32)?;

    let Some(graph) = graph else {
      meta.write_vint(0)?;
      return Ok(());
    };

    meta.write_vint(graph.num_levels()? as i32)?;
    let mut value_count: i64 = 0;
    for level in 0..graph.num_levels()? {
      let nodes_on_level = graph.get_nodes_on_level(level)?;
      value_count += nodes_on_level.size() as i64;

      if level > 0 {
        meta.write_vint(nodes_on_level.size() as i32)?;

        // Completed on-heap graphs and the sorted flush view both yield nodes in order.
        let mut previous = 0;
        for node in nodes_on_level {
          debug_assert!(node >= previous);
          meta.write_vint((node - previous) as i32)?;
          previous = node;
        }
      } else {
        debug_assert_eq!(
          nodes_on_level.size(),
          count as usize,
          "Level 0 expects to have all nodes"
        );
      }
    }

    let start = vector_index.get_file_pointer()?;
    meta.write_long(start as i64)?;

    meta.write_vint(DIRECT_MONOTONIC_BLOCK_SHIFT)?;

    let mut memory_offsets_writer = DirectMonotonicWriter::get_instance(
      meta,
      vector_index,
      value_count,
      DIRECT_MONOTONIC_BLOCK_SHIFT,
    )?;

    let mut cumulative_offset_sum: i64 = 0;

    for &v in graph_level_node_offsets {
      memory_offsets_writer.add(cumulative_offset_sum)?;
      cumulative_offset_sum += v as i64;
    }

    memory_offsets_writer.finish()?;

    let end = vector_index.get_file_pointer()?;
    meta.write_long((end - start) as i64)?;

    Ok(())
  }

  #[allow(clippy::too_many_arguments)]
  fn create_graph_merger<S>(
    field_info: Arc<FieldInfo>,
    scorer_supplier: S,
    m: usize,
    beam_width: usize,
    merge_exec: Option<&Arc<TaskExecutor>>,
    num_merge_workers: usize,
    parallel_merge_task_executor: Option<&Arc<TaskExecutor>>,
    num_parallel_merge_workers: usize,
  ) -> IncrementalHnswGraphMerger<S>
  where
    S: RandomVectorScorerSupplier,
  {
    if let Some(merge_exec) = merge_exec {
      IncrementalHnswGraphMerger::new_with_hook(
        field_info,
        scorer_supplier,
        m,
        beam_width,
        HnswGraphMergerHook::Concurrent(ConcurrentHnswMerger::new(
          Arc::clone(merge_exec),
          num_merge_workers,
        )),
      )
    } else if let Some(parallel_merge_task_executor) = parallel_merge_task_executor {
      IncrementalHnswGraphMerger::new_with_hook(
        field_info,
        scorer_supplier,
        m,
        beam_width,
        HnswGraphMergerHook::Concurrent(ConcurrentHnswMerger::new(
          Arc::clone(parallel_merge_task_executor),
          num_parallel_merge_workers,
        )),
      )
    } else {
      IncrementalHnswGraphMerger::new(field_info, scorer_supplier, m, beam_width)
    }
  }
}

impl<F> Accountable for Lucene99HnswVectorsWriter<F>
where
  F: FlatVectorsWriter,
{
  fn ram_bytes_used(&self) -> Result<i64> {
    let mut size = self
      .flat_vector_writer
      .ram_bytes_used()?
      .saturating_add(size_of_vec(&self.fields));
    for field in &self.fields {
      size = size.saturating_add(field.ram_bytes_used()?);
    }
    Ok(size)
  }
}

impl<F> Closeable for Lucene99HnswVectorsWriter<F>
where
  F: FlatVectorsWriter,
{
  fn close(&mut self) -> Result<()> {
    IOUtils::close_with(0..3, |operation| match operation {
      0 => self.meta.close(),
      1 => self.vector_index.close(),
      _ => self.flat_vector_writer.close(),
    })
  }
}

impl<F> KnnVectorsWriter<F::IndexOutput> for Lucene99HnswVectorsWriter<F>
where
  F: FlatVectorsWriter,
{
  fn add_field<D1, D2>(
    &mut self,
    _write_state: &SegmentWriteState<D1>,
    _segment_info: &SegmentInfo<D2>,
    field_info: Arc<FieldInfo>,
  ) -> Result<usize>
  where
    D1: Directory<IndexOutput = F::IndexOutput>,
  {
    let flat_field_vectors_writer =
      FlatVectorsWriter::flat_add_field(&mut self.flat_vector_writer, field_info.clone())?;
    let scorer = self.flat_vector_writer.get_flat_vector_scorer();
    let v = create_field_writer(
      scorer,
      flat_field_vectors_writer,
      field_info,
      self.m,
      self.beam_width,
      self.info_stream.clone(),
    )?;
    self.fields.push(v);
    Ok(self.fields.len() - 1)
  }

  fn flush<DM>(&mut self, max_doc: i32, sort_map: Option<&DM>) -> Result<()>
  where
    DM: DocMap,
  {
    self
      .flat_vector_writer
      .flat_flush(max_doc, sort_map, &self.fields)?;

    let mut offsets = Vec::new();
    let mut ordinal_maps = smallvec::SmallVec::new();
    for field_idx in 0..self.fields.len() {
      if let Some(sm) = sort_map {
        self.write_sorting_field(field_idx, sm, &mut offsets, &mut ordinal_maps)?;
      } else {
        self.write_field(field_idx, &mut offsets)?;
      }
      offsets.clear();
    }

    Ok(())
  }

  fn merge_one_field<D1, D2, CR>(
    &mut self,
    field_info: &Arc<FieldInfo>,
    merge_state: &MergeState<'_, D1, CR>,
    segment_write_state: &SegmentWriteState<&D2>,
  ) -> Result<()>
  where
    D2: Directory<IndexOutput = F::IndexOutput>,
    CR: CodecReader,
  {
    crate::with_vector_similarity!(*field_info.get_vector_similarity_function(), similarity, {
      let supplier = self.flat_vector_writer.merge_one_field_to_index(
        field_info.as_ref(),
        merge_state,
        segment_write_state,
        similarity,
      )?;
      match supplier {
        CloseableRandomVectorScorerSupplierEnum2::A(supplier) => Self::merge_with_supplier(
          &mut self.vector_index,
          &mut self.meta,
          &mut self.merge_offsets,
          self.m,
          self.beam_width,
          self.merge_exec.as_ref(),
          self.num_merge_workers,
          supplier,
          field_info,
          merge_state,
          segment_write_state,
        ),
        CloseableRandomVectorScorerSupplierEnum2::B(supplier) => Self::merge_with_supplier(
          &mut self.vector_index,
          &mut self.meta,
          &mut self.merge_offsets,
          self.m,
          self.beam_width,
          self.merge_exec.as_ref(),
          self.num_merge_workers,
          supplier,
          field_info,
          merge_state,
          segment_write_state,
        ),
      }
    })
  }

  fn finish(&mut self) -> Result<()> {
    if self.finished {
      return Err(LuceneError::illegal_state("already finished"));
    }
    self.finished = true;

    self.flat_vector_writer.finish()?;
    // write end of fields marker
    self.meta.write_int(-1)?;
    CodecUtil::write_footer(&mut self.meta)?;

    CodecUtil::write_footer(&mut self.vector_index)?;

    Ok(())
  }

  fn add_value<V: VectorInput + ?Sized>(
    &mut self,
    doc_id: i32,
    vector_value: &V,
    field_vectors_writers_idx: usize,
  ) -> Result<()> {
    debug_assert!(field_vectors_writers_idx <= self.fields.len());
    let field_writer = &mut self.fields[field_vectors_writers_idx];
    let flat_field_vectors_writer = self.flat_vector_writer.get_fields_mut();
    field_writer.add_value(doc_id, vector_value, flat_field_vectors_writer)
  }
}

pub(crate) fn dist_func_to_ord(func: &VectorSimilarityFunction) -> Result<u8> {
  for (i, f) in SIMILARITY_FUNCTIONS.iter().enumerate() {
    if f == func {
      return Ok(i as u8);
    }
  }
  Err(LuceneError::illegal_argument(format!(
    "invalid distance function: {:?}",
    func
  )))
}

pub(crate) fn create_field_writer<S>(
  scorer: &S,
  flat_field_vectors_writer_idx: usize,
  field_info: Arc<FieldInfo>,
  m: usize,
  beam_width: usize,
  info_stream: InfoStreamMT,
) -> Result<EncodedFieldWriter<S>>
where
  S: FlatVectorsScorer,
{
  match *field_info.get_vector_similarity_function() {
    VectorSimilarityFunction::Euclidean => create_field_writer_with_similarity(
      scorer,
      flat_field_vectors_writer_idx,
      field_info,
      m,
      beam_width,
      info_stream,
      EuclideanSimilarity,
    )
    .map(EncodedFieldWriter::Euclidean),
    VectorSimilarityFunction::DotProduct => create_field_writer_with_similarity(
      scorer,
      flat_field_vectors_writer_idx,
      field_info,
      m,
      beam_width,
      info_stream,
      DotProductSimilarity,
    )
    .map(EncodedFieldWriter::DotProduct),
    VectorSimilarityFunction::Cosine => create_field_writer_with_similarity(
      scorer,
      flat_field_vectors_writer_idx,
      field_info,
      m,
      beam_width,
      info_stream,
      CosineSimilarity,
    )
    .map(EncodedFieldWriter::Cosine),
    VectorSimilarityFunction::MaximumInnerProduct => create_field_writer_with_similarity(
      scorer,
      flat_field_vectors_writer_idx,
      field_info,
      m,
      beam_width,
      info_stream,
      MaximumInnerProductSimilarity,
    )
    .map(EncodedFieldWriter::MaximumInnerProduct),
  }
}

fn create_field_writer_with_similarity<S, Sim: VectorSimilarity>(
  scorer: &S,
  flat_field_vectors_writer_idx: usize,
  field_info: Arc<FieldInfo>,
  m: usize,
  beam_width: usize,
  info_stream: InfoStreamMT,
  similarity: Sim,
) -> Result<SimilarityFieldWriter<S, Sim>>
where
  S: FlatVectorsScorer,
{
  let dimension = field_info.get_vector_dimension() as usize;
  match field_info.get_vector_encoding() {
    VectorEncoding::BYTE(_) => {
      let supplier = scorer.get_byte_scorer_supplier(similarity, from_bytes(dimension))?;
      FieldWriter::new(
        supplier,
        flat_field_vectors_writer_idx,
        field_info,
        m,
        beam_width,
        info_stream,
      )
      .map(FieldWriterEnum::Byte)
    },
    VectorEncoding::FLOAT32(_) => {
      let supplier = scorer.get_float_scorer_supplier(similarity, from_floats(dimension))?;
      FieldWriter::new(
        supplier,
        flat_field_vectors_writer_idx,
        field_info,
        m,
        beam_width,
        info_stream,
      )
      .map(FieldWriterEnum::Float)
    },
  }
}

pub type SimilarityFieldWriter<S, Sim> = FieldWriterEnum<
  <S as FlatVectorsScorer>::ByteScorerSupplier<ByteVectorValuesImpl, Sim>,
  <S as FlatVectorsScorer>::FloatScorerSupplier<FloatVectorValuesImpl, Sim>,
>;

/// The field chooses its policy once; each variant owns a fully concrete builder.
pub enum EncodedFieldWriter<S: FlatVectorsScorer> {
  Euclidean(SimilarityFieldWriter<S, EuclideanSimilarity>),
  DotProduct(SimilarityFieldWriter<S, DotProductSimilarity>),
  Cosine(SimilarityFieldWriter<S, CosineSimilarity>),
  MaximumInnerProduct(SimilarityFieldWriter<S, MaximumInnerProductSimilarity>),
}

macro_rules! with_similarity_field {
  ($owner:expr, $field:ident, $body:expr) => {
    match $owner {
      Self::Euclidean($field) => $body,
      Self::DotProduct($field) => $body,
      Self::Cosine($field) => $body,
      Self::MaximumInnerProduct($field) => $body,
    }
  };
}

impl<S: FlatVectorsScorer> EncodedFieldWriter<S> {
  pub(crate) fn get_vectors(&self) -> Result<VectorSlices<'_>> {
    with_similarity_field!(self, field, field.get_vectors())
  }
  fn get_docs_with_field_set<'a, W: FlatFieldVectorsWriter>(
    &self,
    writers: &'a mut [W],
  ) -> Result<&'a DocsWithFieldSet> {
    with_similarity_field!(self, field, field.get_docs_with_field_set(writers))
  }
  fn get_graph_and_field_info<W: FlatFieldVectorsWriter>(
    &mut self,
    writers: &mut [W],
  ) -> Result<(&Arc<FieldInfo>, Option<&mut OnHeapHnswGraph>)> {
    with_similarity_field!(self, field, field.get_graph_and_field_info(writers))
  }
}
impl<S: FlatVectorsScorer> Accountable for EncodedFieldWriter<S> {
  fn ram_bytes_used(&self) -> Result<i64> {
    with_similarity_field!(self, field, field.ram_bytes_used())
  }
}
impl<S: FlatVectorsScorer> KnnFieldVectorsWriter for EncodedFieldWriter<S> {
  fn add_value<W: FlatFieldVectorsWriter, V: VectorInput + ?Sized>(
    &mut self,
    doc_id: i32,
    vector_value: &V,
    writers: &mut [W],
  ) -> Result<()> {
    with_similarity_field!(self, field, field.add_value(doc_id, vector_value, writers))
  }
}

/// Dispatches once per field operation, retaining a concrete supplier in each builder.
pub enum FieldWriterEnum<B, F> {
  Byte(FieldWriter<B>),
  Float(FieldWriter<F>),
}
impl<B: BufferedVectorScorerSupplier<Element = u8>, F: BufferedVectorScorerSupplier<Element = f32>>
  FieldWriterEnum<B, F>
{
  pub(crate) fn get_vectors(&self) -> Result<VectorSlices<'_>> {
    match self {
      Self::Byte(w) => w
        .hnsw_graph_builder
        .get_scorer_supplier()
        .get_vector()
        .map(VectorSlices::Byte),
      Self::Float(w) => w
        .hnsw_graph_builder
        .get_scorer_supplier()
        .get_vector()
        .map(VectorSlices::Float),
    }
  }
  fn get_docs_with_field_set<'a, W: FlatFieldVectorsWriter>(
    &self,
    writers: &'a mut [W],
  ) -> Result<&'a DocsWithFieldSet> {
    match self {
      Self::Byte(w) => w.get_docs_with_field_set(writers),
      Self::Float(w) => w.get_docs_with_field_set(writers),
    }
  }
  fn get_graph_and_field_info<W: FlatFieldVectorsWriter>(
    &mut self,
    writers: &mut [W],
  ) -> Result<(&Arc<FieldInfo>, Option<&mut OnHeapHnswGraph>)> {
    match self {
      Self::Byte(w) => w.get_graph_and_field_info(writers),
      Self::Float(w) => w.get_graph_and_field_info(writers),
    }
  }
}
impl<B: BufferedVectorScorerSupplier<Element = u8>, F: BufferedVectorScorerSupplier<Element = f32>>
  Accountable for FieldWriterEnum<B, F>
{
  fn ram_bytes_used(&self) -> Result<i64> {
    match self {
      Self::Byte(w) => w.ram_bytes_used(),
      Self::Float(w) => w.ram_bytes_used(),
    }
  }
}
impl<B: BufferedVectorScorerSupplier<Element = u8>, F: BufferedVectorScorerSupplier<Element = f32>>
  KnnFieldVectorsWriter for FieldWriterEnum<B, F>
{
  fn add_value<W: FlatFieldVectorsWriter, V: VectorInput + ?Sized>(
    &mut self,
    doc_id: i32,
    vector_value: &V,
    writers: &mut [W],
  ) -> Result<()> {
    match self {
      Self::Byte(w) => w.add_value(doc_id, vector_value, writers),
      Self::Float(w) => w.add_value(doc_id, vector_value, writers),
    }
  }
}

pub struct FieldWriter<S> {
  field_info: Arc<FieldInfo>,
  pub(crate) hnsw_graph_builder: DefaultHnswGraphBuilder<S>,
  last_doc_id: i32,
  node: usize,
  flat_field_vectors_writer_idx: usize,
  // Retained vector buffers, updated when the scorer's vector store grows.
  vectors_ram_bytes: i64,
}
impl<S> FieldWriter<S>
where
  S: RandomVectorScorerSupplier,
{
  fn new(
    scorer_supplier: S,
    flat_field_vectors_writer_idx: usize,
    field_info: Arc<FieldInfo>,
    m: usize,
    beam_width: usize,
    info_stream: InfoStreamMT,
  ) -> Result<Self> {
    let mut hnsw_graph_builder = create(scorer_supplier, m, beam_width, rand_seed())?;
    hnsw_graph_builder.set_info_stream(info_stream);

    Ok(Self {
      field_info,
      hnsw_graph_builder,
      last_doc_id: -1,
      node: 0,
      flat_field_vectors_writer_idx,
      vectors_ram_bytes: 0,
    })
  }
}

impl<S> FieldWriter<S>
where
  S: RandomVectorScorerSupplier,
{
  pub fn get_docs_with_field_set<'a, F>(
    &self,
    flat_field_vectors_writers: &'a mut [F],
  ) -> Result<&'a DocsWithFieldSet>
  where
    F: FlatFieldVectorsWriter,
  {
    let v = flat_field_vectors_writers
      .get(self.flat_field_vectors_writer_idx)
      .ok_or_else(|| LuceneError::illegal_state("Invalid flat field vectors writer index"))?;
    Ok(v.get_docs_with_field_set())
  }
  fn get_graph_and_field_info<F>(
    &mut self,
    flat_field_vectors_writers: &mut [F],
  ) -> Result<(&Arc<FieldInfo>, Option<&mut OnHeapHnswGraph>)>
  where
    F: FlatFieldVectorsWriter,
  {
    debug_assert!({
      let v = flat_field_vectors_writers
        .get(self.flat_field_vectors_writer_idx)
        .ok_or_else(|| LuceneError::illegal_state("Invalid flat field vectors writer index"))?;
      v.is_finished()
    });

    let graph = if self.node > 0 {
      Some(self.hnsw_graph_builder.get_completed_graph()?)
    } else {
      None
    };
    Ok((&self.field_info, graph))
  }
}
impl<S> Accountable for FieldWriter<S>
where
  S: RandomVectorScorerSupplier,
{
  fn ram_bytes_used(&self) -> Result<i64> {
    let graph_bytes = self.hnsw_graph_builder.get_graph().ram_bytes_used()?;
    Ok(graph_bytes.saturating_add(self.vectors_ram_bytes))
  }
}

impl<S> KnnFieldVectorsWriter for FieldWriter<S>
where
  S: BufferedVectorScorerSupplier,
{
  fn add_value<F, V: VectorInput + ?Sized>(
    &mut self,
    doc_id: i32,
    vector_value: &V,
    flat_field_vectors_writers: &mut [F],
  ) -> Result<()>
  where
    F: FlatFieldVectorsWriter,
  {
    if doc_id == self.last_doc_id {
      return Err(LuceneError::illegal_argument(format!(
        "VectorValuesField \"{}\" appears more than once in this document (only one value is allowed per field)",
        self.field_info.name
      )));
    }
    let flat_field_vectors_writer = flat_field_vectors_writers
      .get_mut(self.flat_field_vectors_writer_idx)
      .ok_or_else(|| LuceneError::illegal_state("Invalid flat field vectors writer index"))?;
    let ss = self.hnsw_graph_builder.get_scorer_supplier_mut();
    let vectors = ss.get_vector_mut()?;
    let previous_capacity_bytes = size_of_vec(vectors);
    FlatFieldVectorsWriter::flat_add_value(
      flat_field_vectors_writer,
      doc_id,
      vector_value,
      vectors,
    )?;
    self.vectors_ram_bytes = self
      .vectors_ram_bytes
      .saturating_add(size_of_vec(vectors).saturating_sub(previous_capacity_bytes))
      .saturating_add(
        vectors
          .last()
          .map_or(0, |vector| std::mem::size_of_val(vector.as_ref()) as i64),
      );
    self.hnsw_graph_builder.add_graph_node(self.node)?;
    self.node += 1;
    self.last_doc_id = doc_id;
    Ok(())
  }
}

type ReconstructedGraph<'a> = (&'a mut OnHeapHnswGraph, smallvec::SmallVec<[Vec<usize>; 1]>);

struct HnswGraphImpl<'a> {
  graph: &'a mut OnHeapHnswGraph,
  upper_level_nodes: &'a [Vec<usize>],
}
impl<'a> HnswGraphImpl<'a> {
  fn new(graph: &'a mut OnHeapHnswGraph, upper_level_nodes: &'a [Vec<usize>]) -> Self {
    Self {
      graph,
      upper_level_nodes,
    }
  }
}
impl<'a> HnswGraph for HnswGraphImpl<'a> {
  fn seek(&mut self, _level: usize, _target: usize) -> Result<()> {
    Err(LuceneError::unsupported_operation(
      "Not supported on a mock graph",
    ))
  }

  fn size(&self) -> usize {
    self.graph.size()
  }

  fn next_neighbor(&mut self) -> Result<usize> {
    Err(LuceneError::unsupported_operation(
      "Not supported on a mock graph",
    ))
  }

  fn num_levels(&self) -> Result<usize> {
    self.graph.num_levels()
  }

  fn entry_node(&self) -> Result<Option<usize>> {
    Err(LuceneError::unsupported_operation(
      "Not supported on a mock graph",
    ))
  }

  type NodeIterator = NodesIteratorEnum2<
    ArrayNodesIterator<&'a Vec<usize>>,
    <OnHeapHnswGraph as HnswGraph>::NodeIterator,
  >;

  fn get_nodes_on_level(&mut self, level: usize) -> Result<Self::NodeIterator> {
    if level == 0 {
      self.graph.get_nodes_on_level(0).map(NodesIteratorEnum2::B)
    } else {
      let nodes = self
        .upper_level_nodes
        .get(level - 1)
        .ok_or_else(|| LuceneError::illegal_argument(format!("Invalid level: {}", level)))?;
      Ok(NodesIteratorEnum2::A(ArrayNodesIterator::from_nodes(
        Option::from(nodes),
        nodes.len(),
      )))
    }
  }
}

impl<F: FlatVectorsWriter> Lucene99HnswVectorsWriter<F> {
  #[allow(clippy::too_many_arguments)]
  fn merge_with_supplier<D1, D2, CR, S>(
    vector_index: &mut F::IndexOutput,
    meta: &mut F::IndexOutput,
    vector_index_node_offsets: &mut Vec<i32>,
    m: usize,
    beam_width: usize,
    merge_exec: Option<&Arc<TaskExecutor>>,
    num_merge_workers: usize,
    mut scorer_supplier: S,
    field_info: &Arc<FieldInfo>,
    merge_state: &MergeState<'_, D1, CR>,
    segment_write_state: &SegmentWriteState<&D2>,
  ) -> Result<()>
  where
    D2: Directory<IndexOutput = F::IndexOutput>,
    CR: CodecReader,
    S: CloseableRandomVectorScorerSupplier,
  {
    let mut success = false;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      let vector_index_offset = vector_index.get_file_pointer()?;

      let mut graph = None;
      vector_index_node_offsets.clear();

      if scorer_supplier.total_vector_count()? > 0 {
        let mut merger = Self::create_graph_merger(
          field_info.clone(),
          &scorer_supplier,
          m,
          beam_width,
          merge_exec,
          num_merge_workers,
          Some(&merge_state.intra_merge_task_executor),
          num_merge_workers,
        );

        for i in 0..merge_state.live_docs.len() {
          if !has_vector_values(&merge_state.field_infos[i], &field_info.name)? {
            continue;
          }

          if let Some(reader) = merge_state.knn_vectors_readers[i].as_ref() {
            merger.add_reader(i, reader, i, merge_state.live_docs[i].as_ref())?;
          }
        }
        let merged_graph = match field_info.get_vector_encoding() {
          VectorEncoding::BYTE(_) => merger.merge(
            merge_byte_vector_values(field_info.as_ref(), merge_state)?,
            segment_write_state.info_stream.clone(),
            scorer_supplier.total_vector_count()?,
            merge_state.knn_vectors_readers.as_ref(),
            merge_state.doc_maps.as_ref(),
          )?,
          VectorEncoding::FLOAT32(_) => merger.merge(
            merge_float_vector_values(field_info.as_ref(), merge_state)?,
            segment_write_state.info_stream.clone(),
            scorer_supplier.total_vector_count()?,
            merge_state.knn_vectors_readers.as_ref(),
            merge_state.doc_maps.as_ref(),
          )?,
        };

        graph = Some(merged_graph);
        Self::write_graph(vector_index, graph.as_mut(), vector_index_node_offsets)?;
      }

      let vector_index_length = vector_index.get_file_pointer()? - vector_index_offset;
      Self::write_meta(
        vector_index,
        meta,
        m,
        field_info,
        vector_index_offset.try_convert()?,
        vector_index_length.try_convert()?,
        scorer_supplier.total_vector_count()?,
        graph.as_mut(),
        vector_index_node_offsets,
      )?;
      success = true;
      Ok(())
    }));

    if success {
      scorer_supplier.close()?;
    } else {
      IOUtils::close_while_handling_exception(&mut scorer_supplier);
    }
    unwrap_caught_result!(result)
  }
}

use crate::core::index::vector_buffer::VectorSlices;
use crate::core::util::hnsw::random_vector_scorer_supplier::BufferedVectorScorerSupplier;
