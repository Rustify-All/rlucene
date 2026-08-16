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
use crate::core::codecs::hnsw::flat_vectors_writer::FlatVectorsWriter;
use crate::core::codecs::knn_field_vectors_writer::VectorValueEnum;
use crate::core::codecs::knn_vectors_reader::KnnVectorsReader;
use crate::core::codecs::knn_vectors_writer::{
  KnnVectorsWriter, has_vector_values, map_old_ord_to_new_ord, merge_float_vector_values,
};
use crate::core::codecs::lucene95::has_index_slice::HasIndexSlice;
use crate::core::codecs::lucene95::ord_to_doc_disi_reader_configuration::OrdToDocDISIReaderConfiguration;
use crate::core::codecs::lucene99::lucene99_flat_vectors_format::DIRECT_MONOTONIC_BLOCK_SHIFT;
use crate::core::codecs::lucene99::lucene99_hnsw_vectors_writer::{
  DefaultRandomVectorScorerSupplier, FieldWriter as HnswFieldWriter,
};
use crate::core::codecs::lucene99::lucene99_scalar_quantized_vector_scorer::{
  Lucene99ScalarQuantizedVectorScorer, ScalarQuantizedRandomVectorScorerSupplier,
  ScalarQuantizedVectorsScorer,
};
use crate::core::codecs::lucene99::lucene99_scalar_quantized_vectors_format::{
  DYNAMIC_CONFIDENCE_INTERVAL, Lucene99ScalarQuantizedVectorsFormat, META_CODEC_NAME,
  META_EXTENSION, QUANTIZED_VECTOR_COMPONENT, VECTOR_DATA_CODEC_NAME, VECTOR_DATA_EXTENSION,
  VERSION_ADD_BITS, VERSION_CURRENT,
};
use crate::core::codecs::lucene99::off_heap_quantized_byte_vector_values::{
  self, compress_bytes, compressed_array,
};
use crate::core::index::IndexFileNames;
use crate::core::index::byte_vector_values::ByteVectorValues;
use crate::core::index::codec_reader::CRKnnVectorReader;
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::docs_with_field_set::DocsWithFieldSet;
use crate::core::index::dummy::dummy_byte_vector_values::DummyByteVectorValues;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::float_vector_values::{FloatVectorValues, FloatVectorValuesEnum2};
use crate::core::index::knn_vector_values::{
  BitsImpl1, DenseDocIndexIterator, DocIndexIterator, KnnVectorValues,
};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::merge_state::{DocMap as MergeDocMap, MergeState, MergeStateDocMapImpl};
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::sorter::DocMap;
use crate::core::index::vector_encoding::VectorEncoding;
use crate::core::index::vector_similarity_function::VectorSimilarityFunction;
use crate::core::index::{DocIDMerger, DocIDMergerEnum, Sub, SubBase, of};
use crate::core::search::doc_id_set::DocIdSet;
use crate::core::search::doc_id_set_iterator::{DocIdSetIterator, NO_MORE_DOCS};
use crate::core::search::dummy::dummy_vector_scorer::DummyVectorScorer;
use crate::core::store::directory::Directory;
use crate::core::store::{IndexInput, IndexOutput};
use crate::core::util::TryIntoInt;
use crate::core::util::accountable::Accountable;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::bits::Bits;
use crate::core::util::clone::TryClone;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::hnsw::closeable_random_vector_scorer_supplier::{
  CloseableRandomVectorScorerSupplier, CloseableRandomVectorScorerSupplierEnum2,
};
use crate::core::util::hnsw::random_vector_scorer_supplier::RandomVectorScorerSupplier;
use crate::core::util::info_stream::{InfoStream, InfoStreamMT};
use crate::core::util::io_utils::IOUtils;
use crate::core::util::quantization::quantized_byte_vector_values::QuantizedByteVectorValues;
use crate::core::util::quantization::scalar_quantizer::ScalarQuantizer;
use crate::core::util::ram_usage_estimator::size_of_vec;
use crate::core::util::vector_util::VectorUtil;
use std::borrow::Cow;
use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::sync::Arc;

// Used for determining when merged quantiles shifted too far from individual segment quantiles.
// When merging quantiles from various segments, we need to ensure that the new quantiles
// are not exceptionally different from an individual segments quantiles.
// This would imply that the quantization buckets would shift too much
// for floating point values and justify recalculating the quantiles. This helps preserve
// accuracy of the calculated quantiles, even in adversarial cases such as vector clustering.
// This number was determined via empirical testing
const QUANTILE_RECOMPUTE_LIMIT: f32 = 32.0;
// Used for determining if a new quantization state requires a re-quantization
// for a given segment.
// This ensures that in expectation 4/5 of the vector would be unchanged by requantization.
// Furthermore, only those values where the value is within 1/5 of the centre of a quantization
// bin will be changed. In these cases the error introduced by snapping one way or another
// is small compared to the error introduced by quantization in the first place. Furthermore,
// empirical testing showed that the relative error by not requantizing is small (compared to
// the quantization error) and the condition is sensitive enough to detect all adversarial cases,
// such as merging clustered data.
const REQUANTIZATION_LIMIT: f32 = 0.2;

/// Writes quantized vector values and metadata to index segments.
pub struct Lucene99ScalarQuantizedVectorsWriter<O, R, F> {
  fields: Vec<ScalarQuantizedFieldWriter>,
  meta: O,
  quantized_vector_data: O,
  confidence_interval: Option<f32>,
  raw_vector_delegate: R,
  flat_vector_scorer: F,
  bits: u8,
  compress: bool,
  version: i32,
  finished: bool,
  info_stream: InfoStreamMT,
}

impl<O, R, F> Lucene99ScalarQuantizedVectorsWriter<O, R, F>
where
  O: IndexOutput,
  R: FlatVectorsWriter<IndexOutput = O>,
  F: FlatVectorsScorer,
{
  pub fn new<D1, D2>(
    state: &SegmentWriteState<D1>,
    confidence_interval: Option<f32>,
    bits: u8,
    compress: bool,
    raw_vector_delegate: R,
    flat_vector_scorer: F,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self>
  where
    D1: Directory<IndexOutput = O>,
  {
    Self::with_version(
      state,
      VERSION_CURRENT,
      confidence_interval,
      bits,
      compress,
      raw_vector_delegate,
      flat_vector_scorer,
      segment_info,
    )
  }

  #[allow(clippy::too_many_arguments)]
  fn with_version<D1, D2>(
    state: &SegmentWriteState<D1>,
    version: i32,
    confidence_interval: Option<f32>,
    bits: u8,
    compress: bool,
    mut raw_vector_delegate: R,
    flat_vector_scorer: F,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self>
  where
    D1: Directory<IndexOutput = O>,
  {
    let meta_file_name =
      IndexFileNames::segment_file_name(&segment_info.name, &state.segment_suffix, META_EXTENSION);

    let quantized_vector_data_file_name = IndexFileNames::segment_file_name(
      &segment_info.name,
      &state.segment_suffix,
      VECTOR_DATA_EXTENSION,
    );

    let mut meta = None;
    let mut quantized_vector_data = None;
    let mut success = false;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      meta = Some(
        state
          .directory
          .create_output(&meta_file_name, state.context)?,
      );
      quantized_vector_data = Some(
        state
          .directory
          .create_output(&quantized_vector_data_file_name, state.context)?,
      );
      CodecUtil::write_index_header(
        meta
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("meta output is missing"))?,
        META_CODEC_NAME,
        version,
        segment_info.get_id(),
        &state.segment_suffix,
      )?;
      CodecUtil::write_index_header(
        quantized_vector_data
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("quantized vector data output is missing"))?,
        VECTOR_DATA_CODEC_NAME,
        version,
        segment_info.get_id(),
        &state.segment_suffix,
      )?;
      success = true;
      Ok(())
    }));

    if !success {
      IOUtils::close_while_handling_exception((
        meta.as_mut(),
        quantized_vector_data.as_mut(),
        &mut raw_vector_delegate,
      ));
    }
    unwrap_caught_result!(result)?;
    let (meta, quantized_vector_data) = match (meta, quantized_vector_data) {
      (Some(meta), Some(quantized_vector_data)) => (meta, quantized_vector_data),
      (mut meta, mut quantized_vector_data) => {
        IOUtils::close_while_handling_exception((
          meta.as_mut(),
          quantized_vector_data.as_mut(),
          &mut raw_vector_delegate,
        ));
        return Err(LuceneError::illegal_state(
          "scalar quantized outputs are missing after successful construction",
        ));
      },
    };

    Ok(Self {
      fields: Vec::new(),
      meta,
      quantized_vector_data,
      confidence_interval,
      raw_vector_delegate,
      flat_vector_scorer,
      bits,
      compress,
      version,
      finished: false,
      info_stream: state.info_stream.clone(),
    })
  }
  #[allow(clippy::too_many_arguments)]
  fn write_field<FW>(
    meta: &mut O,
    quantized_vector_data: &mut O,
    field_data: &ScalarQuantizedFieldWriter,
    flat_field_vectors_writers: &mut [FW],
    max_doc: i32,
    vectors: &[VectorValueEnum],
    scalar_quantizer: &ScalarQuantizer,
    version: i32,
  ) -> Result<()>
  where
    FW: FlatFieldVectorsWriter,
  {
    // write vector values
    let vector_data_offset = quantized_vector_data.align_file_pointer(BitUtil::FLOAT_BYTES)?;
    write_quantized_vectors(quantized_vector_data, field_data, vectors, scalar_quantizer)?;
    let vector_data_length = quantized_vector_data.get_file_pointer()? - vector_data_offset;

    write_meta(
      meta,
      quantized_vector_data,
      field_data.field_info.as_ref(),
      max_doc,
      vector_data_offset as i64,
      vector_data_length as i64,
      field_data.confidence_interval,
      field_data.bits,
      field_data.compress,
      scalar_quantizer.get_lower_quantile(),
      scalar_quantizer.get_upper_quantile(),
      field_data.get_docs_with_field_set(flat_field_vectors_writers)?,
      version,
    )
  }
  #[allow(clippy::too_many_arguments)]
  fn write_sorting_field<DM, FW>(
    meta: &mut O,
    quantized_vector_data: &mut O,
    field_data: &ScalarQuantizedFieldWriter,
    flat_field_vectors_writers: &mut [FW],
    max_doc: i32,
    sort_map: &DM,
    vectors: &[VectorValueEnum],
    scalar_quantizer: &ScalarQuantizer,
    version: i32,
  ) -> Result<()>
  where
    DM: DocMap,
    FW: FlatFieldVectorsWriter,
  {
    let docs_with_field = field_data.get_docs_with_field_set(flat_field_vectors_writers)?;
    let mut ord_map = vec![0usize; docs_with_field.cardinality() as usize]; // new ord to old ord

    let mut new_docs_with_field = DocsWithFieldSet::new();
    map_old_ord_to_new_ord(
      docs_with_field,
      sort_map,
      None,
      Some(&mut ord_map),
      Some(&mut new_docs_with_field),
    )?;
    new_docs_with_field.finish();

    // write vector values
    let vector_data_offset = quantized_vector_data.align_file_pointer(BitUtil::FLOAT_BYTES)?;
    write_sorted_quantized_vectors(
      quantized_vector_data,
      field_data,
      &ord_map,
      vectors,
      scalar_quantizer,
    )?;
    let quantized_vector_length = quantized_vector_data.get_file_pointer()? - vector_data_offset;
    write_meta(
      meta,
      quantized_vector_data,
      field_data.field_info.as_ref(),
      max_doc,
      vector_data_offset as i64,
      quantized_vector_length as i64,
      field_data.confidence_interval,
      field_data.bits,
      field_data.compress,
      scalar_quantizer.get_lower_quantile(),
      scalar_quantizer.get_upper_quantile(),
      &new_docs_with_field,
      version,
    )
  }
}

impl<O, R, S> Lucene99ScalarQuantizedVectorsWriter<O, R, Lucene99ScalarQuantizedVectorScorer<S>>
where
  O: IndexOutput,
  R: FlatVectorsWriter<IndexOutput = O>,
  S: FlatVectorsScorer + Clone,
{
  fn merge_one_field_to_index_with_quantization_state<'a, D1, D2, CR>(
    &'a mut self,
    segment_write_state: &SegmentWriteState<'a, &'a D2>,
    field_info: &FieldInfo,
    merge_state: &MergeState<'_, D1, CR>,
    merged_quantization_state: ScalarQuantizer,
  ) -> Result<
    <Self as FlatVectorsWriter>::CloseableRandomVectorScorerSupplier<'a, D2::IndexInput, D2>,
  >
  where
    D2: Directory<IndexOutput = O>,
    CR: CodecReader,
  {
    if segment_write_state
      .info_stream
      .is_enabled(QUANTIZED_VECTOR_COMPONENT)
    {
      segment_write_state.info_stream.message(
        QUANTIZED_VECTOR_COMPONENT,
        &format!(
          "quantized field= confidenceInterval={:?} minQuantile={} maxQuantile={}",
          self.confidence_interval,
          merged_quantization_state.get_lower_quantile(),
          merged_quantization_state.get_upper_quantile()
        ),
      )?;
    }
    let vector_data_offset = self
      .quantized_vector_data
      .align_file_pointer(BitUtil::FLOAT_BYTES)?;
    let mut temp_quantized_vector_data = segment_write_state.directory.create_temp_output(
      self.quantized_vector_data.get_name(),
      "temp",
      segment_write_state.context,
    )?;
    let temp_quantized_vector_name = temp_quantized_vector_data.get_name().to_string();
    let mut quantization_data_input = None;
    let mut success = false;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
      || -> Result<
        <Self as FlatVectorsWriter>::CloseableRandomVectorScorerSupplier<'_, D2::IndexInput, D2>,
      > {
        let byte_vector_values = MergedQuantizedVectorValues::merge_quantized_byte_vector_values(
          field_info,
          merge_state,
          merged_quantization_state.clone(),
        )?;
        let docs_with_field = write_quantized_vector_data(
          &mut temp_quantized_vector_data,
          &byte_vector_values,
          self.bits,
          self.compress,
        )?;
        CodecUtil::write_footer(&mut temp_quantized_vector_data)?;
        IOUtils::close_one(&mut temp_quantized_vector_data)?;

        quantization_data_input = Some(
          segment_write_state
            .directory
            .open_input(&temp_quantized_vector_name, segment_write_state.context)?,
        );
        let quantization_data_input_ref = quantization_data_input
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("quantization data input is missing"))?;
        let copy_len = quantization_data_input_ref.length()? - CodecUtil::footer_length();
        self
          .quantized_vector_data
          .copy_bytes(quantization_data_input_ref, copy_len)?;
        let vector_data_length = self.quantized_vector_data.get_file_pointer()? - vector_data_offset;
        CodecUtil::retrieve_checksum(quantization_data_input_ref)?;
        write_meta(
          &mut self.meta,
          &mut self.quantized_vector_data,
          field_info,
          merge_state.segment_info.max_doc()?,
          vector_data_offset as i64,
          vector_data_length as i64,
          self.confidence_interval,
          self.bits,
          self.compress,
          merged_quantization_state.get_lower_quantile(),
          merged_quantization_state.get_upper_quantile(),
          &docs_with_field,
          self.version,
        )?;
        success = true;

        let vector_values_input = quantization_data_input_ref.try_clone()?;
        let random_vector_scorer_supplier =
          self.flat_vector_scorer.get_random_vector_scorer_supplier_quantized(
            *field_info.get_vector_similarity_function(),
            off_heap_quantized_byte_vector_values::DenseOffHeapVectorValues::new(
              field_info.get_vector_dimension() as usize,
              docs_with_field.cardinality() as usize,
              merged_quantization_state,
              self.compress,
              *field_info.get_vector_similarity_function(),
              self.flat_vector_scorer.clone(),
              vector_values_input,
            ),
          )?;
        let quantization_data_input = quantization_data_input
          .take()
          .ok_or_else(|| LuceneError::illegal_state("quantization data input is missing"))?;
        Ok(CloseableRandomVectorScorerSupplierEnum2::B(
          ScalarQuantizedCloseableRandomVectorScorerSupplier::new_quantized(
            docs_with_field.cardinality(),
            random_vector_scorer_supplier,
            segment_write_state.directory,
            temp_quantized_vector_name.clone(),
            quantization_data_input,
          ),
        ))
      },
    ));

    if !success {
      IOUtils::close_while_handling_exception((
        &mut temp_quantized_vector_data,
        quantization_data_input.as_ref(),
      ));
      IOUtils::delete_files_ignoring_exceptions(
        segment_write_state.directory,
        std::iter::once(&temp_quantized_vector_name),
      );
    }
    unwrap_caught_result!(result)
  }
}

impl<O, R, F> Accountable for Lucene99ScalarQuantizedVectorsWriter<O, R, F>
where
  R: Accountable,
{
  fn ram_bytes_used(&self) -> Result<i64> {
    let mut total = self
      .raw_vector_delegate
      .ram_bytes_used()?
      .saturating_add(size_of_vec(&self.fields));
    for field in &self.fields {
      total = total.saturating_add(field.ram_bytes_used()?);
    }
    Ok(total)
  }
}

impl<O, R, F> Closeable for Lucene99ScalarQuantizedVectorsWriter<O, R, F>
where
  O: Closeable,
  R: Closeable,
{
  fn close(&mut self) -> Result<()> {
    IOUtils::close(0..3, |operation| match operation {
      0 => self.meta.close(),
      1 => self.quantized_vector_data.close(),
      2 => self.raw_vector_delegate.close(),
      _ => unreachable!(),
    })
  }
}

impl<O, R, S> KnnVectorsWriter<O>
  for Lucene99ScalarQuantizedVectorsWriter<O, R, Lucene99ScalarQuantizedVectorScorer<S>>
where
  O: IndexOutput,
  R: FlatVectorsWriter<IndexOutput = O>,
  S: FlatVectorsScorer + Clone,
{
  fn add_field<D1, D2>(
    &mut self,
    _write_state: &SegmentWriteState<D1>,
    _segment_info: &SegmentInfo<D2>,
    field_info: Arc<FieldInfo>,
  ) -> Result<usize>
  where
    D1: Directory<IndexOutput = O>,
  {
    self.flat_add_field(field_info)
  }

  fn flush<DM>(&mut self, max_doc: i32, sort_map: Option<&DM>) -> Result<()>
  where
    DM: DocMap,
  {
    self.raw_vector_delegate.flush(max_doc, sort_map)?;

    for field_idx in 0..self.fields.len() {
      let field = &self.fields[field_idx];
      let vectors = {
        let flat_field_vectors_writers = self.raw_vector_delegate.get_fields_mut();
        flat_field_vectors_writers
          .get(field.flat_field_vectors_writer_idx)
          .ok_or_else(|| LuceneError::illegal_state("Invalid flat field vectors writer index"))?
          .get_vectors()?
      };
      let scalar_quantizer = {
        let flat_field_vectors_writers = self.raw_vector_delegate.get_fields_mut();
        field.create_quantizer(flat_field_vectors_writers, vectors.as_ref())?
      };
      let flat_field_vectors_writers = self.raw_vector_delegate.get_fields_mut();
      if let Some(sort_map) = sort_map {
        Self::write_sorting_field(
          &mut self.meta,
          &mut self.quantized_vector_data,
          field,
          flat_field_vectors_writers,
          max_doc,
          sort_map,
          vectors.as_ref(),
          &scalar_quantizer,
          self.version,
        )?;
      } else {
        Self::write_field(
          &mut self.meta,
          &mut self.quantized_vector_data,
          field,
          flat_field_vectors_writers,
          max_doc,
          vectors.as_ref(),
          &scalar_quantizer,
          self.version,
        )?;
      }
      self.fields[field_idx].finish(self.raw_vector_delegate.get_fields_mut())?;
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
    D2: Directory<IndexOutput = O>,
    CR: CodecReader,
  {
    self
      .raw_vector_delegate
      .merge_one_field(field_info, merge_state, segment_write_state)?;
    // Since we know we will not be searching for additional indexing, we can just write the
    // the vectors directly to the new segment.
    // No need to use temporary file as we don't have to re-open for reading
    if *field_info.get_vector_encoding() == VectorEncoding::FLOAT32(BitUtil::FLOAT_BYTES) {
      let merged_quantization_state = merge_and_recalculate_quantiles(
        merge_state,
        field_info.as_ref(),
        self.confidence_interval,
        self.bits,
      )?;
      let byte_vector_values = MergedQuantizedVectorValues::merge_quantized_byte_vector_values(
        field_info.as_ref(),
        merge_state,
        merged_quantization_state.clone(),
      )?;
      let vector_data_offset = self
        .quantized_vector_data
        .align_file_pointer(BitUtil::FLOAT_BYTES)?;
      let docs_with_field = write_quantized_vector_data(
        &mut self.quantized_vector_data,
        &byte_vector_values,
        self.bits,
        self.compress,
      )?;
      let vector_data_length = self.quantized_vector_data.get_file_pointer()? - vector_data_offset;
      write_meta(
        &mut self.meta,
        &mut self.quantized_vector_data,
        field_info.as_ref(),
        merge_state.segment_info.max_doc()?,
        vector_data_offset as i64,
        vector_data_length as i64,
        self.confidence_interval,
        self.bits,
        self.compress,
        merged_quantization_state.get_lower_quantile(),
        merged_quantization_state.get_upper_quantile(),
        &docs_with_field,
        self.version,
      )?;
    }
    Ok(())
  }

  fn finish(&mut self) -> Result<()> {
    if self.finished {
      return Err(LuceneError::illegal_state("already finished"));
    }
    self.finished = true;
    self.raw_vector_delegate.finish()?;
    // write end of fields marker
    self.meta.write_int(-1)?;
    CodecUtil::write_footer(&mut self.meta)?;

    CodecUtil::write_footer(&mut self.quantized_vector_data)?;

    Ok(())
  }

  fn add_value(
    &mut self,
    doc_id: i32,
    vector_value: &VectorValueEnum,
    field_vectors_writers_idx: usize,
  ) -> Result<()> {
    self
      .raw_vector_delegate
      .add_value(doc_id, vector_value, field_vectors_writers_idx)
  }
}

impl<O, R, S> FlatVectorsWriter
  for Lucene99ScalarQuantizedVectorsWriter<O, R, Lucene99ScalarQuantizedVectorScorer<S>>
where
  O: IndexOutput,
  R: FlatVectorsWriter<IndexOutput = O>,
  S: FlatVectorsScorer + Clone,
{
  type IndexOutput = O;
  type FlatVectorsScorer = Lucene99ScalarQuantizedVectorScorer<S>;

  fn get_flat_vector_scorer(&self) -> &Self::FlatVectorsScorer {
    &self.flat_vector_scorer
  }

  fn flat_add_field(&mut self, field_info: Arc<FieldInfo>) -> Result<usize> {
    let raw_vector_delegate = self
      .raw_vector_delegate
      .flat_add_field(field_info.clone())?;
    if *field_info.get_vector_encoding() == VectorEncoding::FLOAT32(BitUtil::FLOAT_BYTES) {
      if self.bits <= 4 && field_info.get_vector_dimension() % 2 != 0 {
        return Err(LuceneError::illegal_argument(format!(
          "bits={} is not supported for odd vector dimensions; vector dimension={}",
          self.bits,
          field_info.get_vector_dimension()
        )));
      }
      let quantized_writer = ScalarQuantizedFieldWriter::new(
        self.confidence_interval,
        self.bits,
        self.compress,
        field_info,
        self.info_stream.clone(),
        raw_vector_delegate,
      );
      self.fields.push(quantized_writer);
    }
    Ok(raw_vector_delegate)
  }

  fn flat_flush<DM, F1>(
    &mut self,
    max_doc: i32,
    sort_map: Option<&DM>,
    fields: &[HnswFieldWriter<DefaultRandomVectorScorerSupplier<F1>>],
  ) -> Result<()>
  where
    DM: DocMap,
    F1: FlatVectorsWriter,
  {
    self
      .raw_vector_delegate
      .flat_flush::<DM, F1>(max_doc, sort_map, fields)?;

    for field_idx in 0..self.fields.len() {
      let field = &self.fields[field_idx];
      let vectors = fields
        .get(field.flat_field_vectors_writer_idx)
        .ok_or_else(|| LuceneError::illegal_state("Invalid flat field vectors writer index"))?
        .hnsw_graph_builder
        .get_scorer_supplier()
        .get_vector()?;
      let scalar_quantizer = {
        let flat_field_vectors_writers = self.raw_vector_delegate.get_fields_mut();
        field.create_quantizer(flat_field_vectors_writers, vectors)?
      };
      let flat_field_vectors_writers = self.raw_vector_delegate.get_fields_mut();
      if let Some(sm) = sort_map {
        Self::write_sorting_field(
          &mut self.meta,
          &mut self.quantized_vector_data,
          field,
          flat_field_vectors_writers,
          max_doc,
          sm,
          vectors,
          &scalar_quantizer,
          self.version,
        )?;
      } else {
        Self::write_field(
          &mut self.meta,
          &mut self.quantized_vector_data,
          field,
          flat_field_vectors_writers,
          max_doc,
          vectors,
          &scalar_quantizer,
          self.version,
        )?;
      }
      self.fields[field_idx].finish(self.raw_vector_delegate.get_fields_mut())?;
    }

    Ok(())
  }

  type FlatFieldVectorsWriter = R::FlatFieldVectorsWriter;

  fn get_fields_mut(&mut self) -> &mut [Self::FlatFieldVectorsWriter] {
    self.raw_vector_delegate.get_fields_mut()
  }

  type CloseableRandomVectorScorerSupplier<'a, I, D>
    = CloseableRandomVectorScorerSupplierEnum2<
    R::CloseableRandomVectorScorerSupplier<'a, I, D>,
    ScalarQuantizedCloseableRandomVectorScorerSupplier<
      'a,
      ScalarQuantizedRandomVectorScorerSupplier<
        off_heap_quantized_byte_vector_values::DenseOffHeapVectorValues<
          I,
          Lucene99ScalarQuantizedVectorScorer<S>,
        >,
      >,
      D,
      I,
    >,
  >
  where
    I: IndexInput + 'a,
    D: Directory,
    Self: 'a,
    D: 'a,
    I: 'a;

  fn merge_one_field_to_index<'a, D1, D2, CR>(
    &'a mut self,
    field_info: &FieldInfo,
    merge_state: &MergeState<'_, D1, CR>,
    segment_write_state: &SegmentWriteState<'a, &'a D2>,
  ) -> Result<Self::CloseableRandomVectorScorerSupplier<'a, D2::IndexInput, D2>>
  where
    D2: Directory<IndexOutput = Self::IndexOutput>,
    CR: CodecReader,
  {
    if *field_info.get_vector_encoding() == VectorEncoding::FLOAT32(BitUtil::FLOAT_BYTES) {
      // Simply merge the underlying delegate, which just copies the raw vector data to a new
      // segment file
      let field_info_arc = merge_state
        .merge_field_infos
        .field_info_by_name(&field_info.name)?
        .ok_or_else(|| {
          LuceneError::illegal_argument(format!("field=\"{}\" not found", field_info.name))
        })?;
      self.raw_vector_delegate.merge_one_field(
        &field_info_arc,
        merge_state,
        segment_write_state,
      )?;
      let merged_quantization_state = merge_and_recalculate_quantiles(
        merge_state,
        field_info,
        self.confidence_interval,
        self.bits,
      )?;
      return self.merge_one_field_to_index_with_quantization_state(
        segment_write_state,
        field_info,
        merge_state,
        merged_quantization_state,
      );
    }
    // We only merge the delegate, since the field type isn't float32, quantization wasn't
    // supported, so bypass it.
    self
      .raw_vector_delegate
      .merge_one_field_to_index(field_info, merge_state, segment_write_state)
      .map(CloseableRandomVectorScorerSupplierEnum2::A)
  }
}

#[allow(clippy::too_many_arguments)]
fn write_meta<O>(
  meta: &mut O,
  quantized_vector_data: &mut O,
  field: &FieldInfo,
  max_doc: i32,
  vector_data_offset: i64,
  vector_data_length: i64,
  confidence_interval: Option<f32>,
  bits: u8,
  compress: bool,
  lower_quantile: f32,
  upper_quantile: f32,
  docs_with_field: &DocsWithFieldSet,
  version: i32,
) -> Result<()>
where
  O: IndexOutput,
{
  meta.write_int(field.number)?;
  meta.write_int(field.get_vector_encoding().ordinal())?;
  meta.write_int(field.get_vector_similarity_function().ordinal())?;
  meta.write_vlong(vector_data_offset)?;
  meta.write_vlong(vector_data_length)?;
  meta.write_vint(field.get_vector_dimension())?;
  let count = docs_with_field.cardinality();
  meta.write_int(count)?;
  if count > 0 {
    debug_assert!(lower_quantile.is_finite() && upper_quantile.is_finite());
    if version >= VERSION_ADD_BITS {
      meta.write_int(
        confidence_interval
          .map(|value| value.to_bits() as i32)
          .unwrap_or(-1),
      )?;
      meta.write_byte(bits)?;
      meta.write_byte(if compress { 1 } else { 0 })?;
    } else {
      debug_assert!(
        confidence_interval.is_none() || confidence_interval != Some(DYNAMIC_CONFIDENCE_INTERVAL)
      );
      let confidence_interval = confidence_interval.unwrap_or_else(|| {
        Lucene99ScalarQuantizedVectorsFormat::calculate_default_confidence_interval(
          field.get_vector_dimension() as usize,
        )
      });
      meta.write_int(confidence_interval.to_bits() as i32)?;
    }
    meta.write_int(lower_quantile.to_bits() as i32)?;
    meta.write_int(upper_quantile.to_bits() as i32)?;
  }
  // write docIDs
  OrdToDocDISIReaderConfiguration::write_stored_meta(
    DIRECT_MONOTONIC_BLOCK_SHIFT,
    meta,
    quantized_vector_data,
    count,
    max_doc,
    docs_with_field,
  )
}

fn write_quantized_vectors<O>(
  quantized_vector_data: &mut O,
  field_data: &ScalarQuantizedFieldWriter,
  vectors: &[VectorValueEnum],
  scalar_quantizer: &ScalarQuantizer,
) -> Result<()>
where
  O: IndexOutput,
{
  let mut vector = vec![0u8; field_data.field_info.get_vector_dimension() as usize];
  let mut compressed_vector = if field_data.compress {
    compressed_array(
      field_data.field_info.get_vector_dimension() as usize,
      field_data.bits,
    )
  } else {
    None
  };
  let mut copy = if field_data.normalize {
    Some(vec![
      0f32;
      field_data.field_info.get_vector_dimension() as usize
    ])
  } else {
    None
  };
  debug_assert!(vectors.is_empty() || scalar_quantizer.get_bits() == field_data.bits);
  for v in vectors {
    let borrowed;
    let vector_value = if field_data.normalize {
      let copy = copy
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("missing normalized vector buffer"))?;
      copy.copy_from_slice(v.as_floats()?);
      VectorUtil::l2normalize(copy)?;
      borrowed = copy.as_slice();
      borrowed
    } else {
      v.as_floats()?
    };

    let offset_correction = scalar_quantizer.quantize(
      vector_value,
      &mut vector,
      *field_data.field_info.get_vector_similarity_function(),
    );
    if let Some(compressed_vector) = compressed_vector.as_mut() {
      compress_bytes(&vector, compressed_vector)?;
      quantized_vector_data.write_bytes_range(compressed_vector, 0, compressed_vector.len())?;
    } else {
      quantized_vector_data.write_bytes_range(&vector, 0, vector.len())?;
    }
    let offset_buffer = offset_correction.to_le_bytes();
    quantized_vector_data.write_bytes_range(&offset_buffer, 0, offset_buffer.len())?;
  }
  Ok(())
}

fn write_sorted_quantized_vectors<O>(
  quantized_vector_data: &mut O,
  field_data: &ScalarQuantizedFieldWriter,
  ord_map: &[usize],
  vectors: &[VectorValueEnum],
  scalar_quantizer: &ScalarQuantizer,
) -> Result<()>
where
  O: IndexOutput,
{
  let mut vector = vec![0u8; field_data.field_info.get_vector_dimension() as usize];
  let mut compressed_vector = if field_data.compress {
    compressed_array(
      field_data.field_info.get_vector_dimension() as usize,
      field_data.bits,
    )
  } else {
    None
  };
  let mut copy = if field_data.normalize {
    Some(vec![
      0f32;
      field_data.field_info.get_vector_dimension() as usize
    ])
  } else {
    None
  };
  for &ordinal in ord_map {
    let v = vectors
      .get(ordinal)
      .ok_or_else(|| LuceneError::illegal_state("Invalid vector ordinal"))?;
    let borrowed;
    let vector_value = if field_data.normalize {
      let copy = copy
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("missing normalized vector buffer"))?;
      copy.copy_from_slice(v.as_floats()?);
      VectorUtil::l2normalize(copy)?;
      borrowed = copy.as_slice();
      borrowed
    } else {
      v.as_floats()?
    };
    let offset_correction = scalar_quantizer.quantize(
      vector_value,
      &mut vector,
      *field_data.field_info.get_vector_similarity_function(),
    );
    if let Some(compressed_vector) = compressed_vector.as_mut() {
      compress_bytes(&vector, compressed_vector)?;
      quantized_vector_data.write_bytes_range(compressed_vector, 0, compressed_vector.len())?;
    } else {
      quantized_vector_data.write_bytes_range(&vector, 0, vector.len())?;
    }
    let offset_buffer = offset_correction.to_le_bytes();
    quantized_vector_data.write_bytes_range(&offset_buffer, 0, offset_buffer.len())?;
  }
  Ok(())
}

fn merge_quantiles(
  quantization_states: &[Option<ScalarQuantizer>],
  segment_sizes: &[usize],
  bits: u8,
) -> Result<Option<ScalarQuantizer>> {
  debug_assert_eq!(quantization_states.len(), segment_sizes.len());
  if quantization_states.is_empty() {
    return Ok(None);
  }
  let mut lower_quantile = 0.0f32;
  let mut upper_quantile = 0.0f32;
  let mut total_count = 0usize;
  for i in 0..quantization_states.len() {
    let Some(quantization_state) = quantization_states[i].as_ref() else {
      return Ok(None);
    };
    lower_quantile += quantization_state.get_lower_quantile() * segment_sizes[i] as f32;
    upper_quantile += quantization_state.get_upper_quantile() * segment_sizes[i] as f32;
    total_count += segment_sizes[i];
    if quantization_state.get_bits() != bits {
      return Ok(None);
    }
  }
  lower_quantile /= total_count as f32;
  upper_quantile /= total_count as f32;
  ScalarQuantizer::new(lower_quantile, upper_quantile, bits).map(Some)
}

/// Returns true if the quantiles of the merged state are too far from the quantiles of the
/// individual states.
///
/// - `merged_quantization_state`: The merged quantization state
/// - `quantization_states`: The quantization states of the individual segments
///
/// Returns true if the quantiles should be recomputed.
fn should_recompute_quantiles(
  merged_quantization_state: &ScalarQuantizer,
  quantization_states: &[Option<ScalarQuantizer>],
) -> bool {
  // calculate the limit for the quantiles to be considered too far apart
  // We utilize upper & lower here to determine if the new upper and merged upper would
  // drastically
  // change the quantization buckets for floats
  // This is a fairly conservative check.
  let limit = (merged_quantization_state.get_upper_quantile()
    - merged_quantization_state.get_lower_quantile())
    / QUANTILE_RECOMPUTE_LIMIT;
  for quantization_state in quantization_states {
    let Some(quantization_state) = quantization_state.as_ref() else {
      debug_assert!(
        false,
        "missing quantization state after quantiles were merged"
      );
      continue;
    };
    if (quantization_state.get_upper_quantile() - merged_quantization_state.get_upper_quantile())
      .abs()
      > limit
    {
      return true;
    }
    if (quantization_state.get_lower_quantile() - merged_quantization_state.get_lower_quantile())
      .abs()
      > limit
    {
      return true;
    }
  }
  false
}

/// Merges the quantiles of the segments and recalculates the quantiles if necessary.
///
/// - `merge_state`: The merge state
/// - `field_info`: The field info
/// - `confidence_interval`: The confidence interval
/// - `bits`: The number of bits
///
/// Returns the merged quantiles.
pub fn merge_and_recalculate_quantiles<D, CR>(
  merge_state: &MergeState<'_, D, CR>,
  field_info: &FieldInfo,
  confidence_interval: Option<f32>,
  bits: u8,
) -> Result<ScalarQuantizer>
where
  CR: CodecReader,
{
  debug_assert_eq!(
    *field_info.get_vector_encoding(),
    VectorEncoding::FLOAT32(BitUtil::FLOAT_BYTES)
  );
  let mut quantization_states = Vec::with_capacity(merge_state.live_docs.len());
  let mut segment_sizes = Vec::with_capacity(merge_state.live_docs.len());
  for i in 0..merge_state.live_docs.len() {
    if has_vector_values(&merge_state.field_infos[i], &field_info.name)?
      && let Some(knn_vectors_reader) = merge_state.knn_vectors_readers[i].as_ref()
    {
      let fvv = knn_vectors_reader.get_float_vector_values(&field_info.name)?;
      if fvv.size() > 0 {
        let quantization_state = knn_vectors_reader.get_quantization_state(&field_info.name)?;
        // If we have quantization state, we can utilize that to make merging cheaper
        quantization_states.push(quantization_state);
        segment_sizes.push(fvv.size());
      }
    }
  }
  let merged_quantiles = merge_quantiles(&quantization_states, &segment_sizes, bits)?;
  // Segments no providing quantization state indicates that their quantiles were never
  // calculated.
  // To be safe, we should always recalculate given a sample set over all the float vectors in the
  // merged
  // segment view
  let should_recalculate = match merged_quantiles.as_ref() {
    None => true,
    Some(merged_quantiles) => {
      // For smaller `bits` values, we should always recalculate the quantiles
      // TODO: this is very conservative, could we reuse information for even int4 quantization?
      if bits <= 4 {
        true
      } else {
        should_recompute_quantiles(merged_quantiles, &quantization_states)
      }
    },
  };
  if should_recalculate {
    let mut num_vectors = 0usize;
    let float_vector_values = merge_float_vector_values(field_info, merge_state)?;
    let mut iter = float_vector_values.iterator()?;
    // iterate vectorValues and increment numVectors
    loop {
      let doc = iter.next_doc()?;
      if doc == NO_MORE_DOCS {
        break;
      }
      num_vectors += 1;
    }
    return build_scalar_quantizer(
      merge_float_vector_values(field_info, merge_state)?,
      num_vectors,
      *field_info.get_vector_similarity_function(),
      confidence_interval,
      bits,
    );
  }
  merged_quantiles.ok_or_else(|| LuceneError::illegal_state("missing merged quantiles"))
}

/// Returns true if the quantiles of the new quantization state are too far from the quantiles of
/// the existing quantization state. This would imply that floating point values would slightly
/// shift quantization buckets.
///
/// - `existing_quantiles`: The existing quantiles for a segment
/// - `new_quantiles`: The new quantiles for a segment, could be merged, or fully re-calculated
///
/// Returns true if the floating point values should be requantized.
fn should_requantize(
  existing_quantiles: &ScalarQuantizer,
  new_quantiles: &ScalarQuantizer,
) -> bool {
  let tol = REQUANTIZATION_LIMIT
    * (new_quantiles.get_upper_quantile() - new_quantiles.get_lower_quantile())
    / 128.0;
  if (existing_quantiles.get_upper_quantile() - new_quantiles.get_upper_quantile()).abs() > tol {
    return true;
  }
  (existing_quantiles.get_lower_quantile() - new_quantiles.get_lower_quantile()).abs() > tol
}

/// Writes the vector values to the output and returns a set of documents that contains vectors.
pub fn write_quantized_vector_data<O, Q>(
  output: &mut O,
  quantized_byte_vector_values: &Q,
  bits: u8,
  compress: bool,
) -> Result<DocsWithFieldSet>
where
  O: IndexOutput,
  Q: QuantizedByteVectorValues,
{
  let mut docs_with_field = DocsWithFieldSet::new();
  let mut compressed_vector = if compress {
    compressed_array(quantized_byte_vector_values.dimension(), bits)
  } else {
    None
  };
  let mut iter = quantized_byte_vector_values.iterator()?;
  loop {
    let doc = iter.next_doc()?;
    if doc == NO_MORE_DOCS {
      break;
    }
    // write vector
    let ord: usize = iter.index()?.try_convert()?;
    let binary_value = quantized_byte_vector_values.vector_value(ord)?;
    let binary_value = binary_value.as_bytes()?;
    debug_assert_eq!(
      binary_value.len(),
      quantized_byte_vector_values.dimension(),
      "dim={} len={}",
      quantized_byte_vector_values.dimension(),
      binary_value.len()
    );
    if let Some(compressed_vector) = compressed_vector.as_mut() {
      compress_bytes(binary_value, compressed_vector)?;
      output.write_bytes_range(compressed_vector, 0, compressed_vector.len())?;
    } else {
      output.write_bytes_range(binary_value, 0, binary_value.len())?;
    }
    output.write_int(
      quantized_byte_vector_values
        .get_score_correction_constant(ord)?
        .to_bits() as i32,
    )?;
    docs_with_field.add(doc)?;
  }
  docs_with_field.finish();
  Ok(docs_with_field)
}

pub struct ScalarQuantizedFieldWriter {
  field_info: Arc<FieldInfo>,
  confidence_interval: Option<f32>,
  bits: u8,
  compress: bool,
  info_stream: InfoStreamMT,
  normalize: bool,
  finished: bool,
  flat_field_vectors_writer_idx: usize,
}

impl ScalarQuantizedFieldWriter {
  fn new(
    confidence_interval: Option<f32>,
    bits: u8,
    compress: bool,
    field_info: Arc<FieldInfo>,
    info_stream: InfoStreamMT,
    flat_field_vectors_writer_idx: usize,
  ) -> Self {
    Self {
      confidence_interval,
      bits,
      normalize: *field_info.get_vector_similarity_function() == VectorSimilarityFunction::Cosine,
      field_info,
      info_stream,
      compress,
      finished: false,
      flat_field_vectors_writer_idx,
    }
  }

  fn is_finished<FW>(&self, flat_field_vectors_writers: &mut [FW]) -> Result<bool>
  where
    FW: FlatFieldVectorsWriter,
  {
    Ok(
      self.finished && {
        let flat_field_vectors_writer = flat_field_vectors_writers
          .get(self.flat_field_vectors_writer_idx)
          .ok_or_else(|| LuceneError::illegal_state("Invalid flat field vectors writer index"))?;
        flat_field_vectors_writer.is_finished()
      },
    )
  }

  fn finish<FW>(&mut self, flat_field_vectors_writers: &mut [FW]) -> Result<()>
  where
    FW: FlatFieldVectorsWriter,
  {
    if self.finished {
      return Ok(());
    }
    debug_assert!({
      let flat_field_vectors_writer = flat_field_vectors_writers
        .get(self.flat_field_vectors_writer_idx)
        .ok_or_else(|| LuceneError::illegal_state("Invalid flat field vectors writer index"))?;
      flat_field_vectors_writer.is_finished()
    });
    self.finished = true;
    Ok(())
  }

  fn create_quantizer<FW>(
    &self,
    flat_field_vectors_writers: &mut [FW],
    vectors: &[VectorValueEnum],
  ) -> Result<ScalarQuantizer>
  where
    FW: FlatFieldVectorsWriter,
  {
    debug_assert!({
      let flat_field_vectors_writer = flat_field_vectors_writers
        .get(self.flat_field_vectors_writer_idx)
        .ok_or_else(|| LuceneError::illegal_state("Invalid flat field vectors writer index"))?;
      flat_field_vectors_writer.is_finished()
    });
    if vectors.is_empty() {
      return ScalarQuantizer::new(0.0, 0.0, self.bits);
    }
    let quantizer = build_scalar_quantizer(
      FloatVectorWrapper::new(vectors),
      vectors.len(),
      *self.field_info.get_vector_similarity_function(),
      self.confidence_interval,
      self.bits,
    )?;
    if self.info_stream.is_enabled(QUANTIZED_VECTOR_COMPONENT) {
      self.info_stream.message(
        QUANTIZED_VECTOR_COMPONENT,
        &format!(
          "quantized field= confidenceInterval={:?} bits={} minQuantile={} maxQuantile={}",
          self.confidence_interval,
          self.bits,
          quantizer.get_lower_quantile(),
          quantizer.get_upper_quantile()
        ),
      )?;
    }
    Ok(quantizer)
  }

  fn get_docs_with_field_set<'a, FW>(
    &self,
    flat_field_vectors_writers: &'a mut [FW],
  ) -> Result<&'a DocsWithFieldSet>
  where
    FW: FlatFieldVectorsWriter,
  {
    let flat_field_vectors_writer = flat_field_vectors_writers
      .get(self.flat_field_vectors_writer_idx)
      .ok_or_else(|| LuceneError::illegal_state("Invalid flat field vectors writer index"))?;
    Ok(flat_field_vectors_writer.get_docs_with_field_set())
  }
}

impl Accountable for ScalarQuantizedFieldWriter {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(0)
  }
}

pub(crate) fn build_scalar_quantizer<FVV>(
  float_vector_values: FVV,
  num_vectors: usize,
  vector_similarity_function: VectorSimilarityFunction,
  confidence_interval: Option<f32>,
  bits: u8,
) -> Result<ScalarQuantizer>
where
  FVV: FloatVectorValues,
{
  if vector_similarity_function == VectorSimilarityFunction::Cosine {
    let float_vector_values = NormalizedFloatVectorValues::new(float_vector_values);
    if confidence_interval == Some(DYNAMIC_CONFIDENCE_INTERVAL) {
      return ScalarQuantizer::from_vectors_auto_interval(
        &float_vector_values,
        VectorSimilarityFunction::DotProduct,
        num_vectors,
        bits,
      );
    }
    return ScalarQuantizer::from_vectors(
      &float_vector_values,
      confidence_interval.unwrap_or_else(|| {
        Lucene99ScalarQuantizedVectorsFormat::calculate_default_confidence_interval(
          float_vector_values.dimension(),
        )
      }),
      num_vectors,
      bits,
    );
  }
  if confidence_interval == Some(DYNAMIC_CONFIDENCE_INTERVAL) {
    return ScalarQuantizer::from_vectors_auto_interval(
      &float_vector_values,
      vector_similarity_function,
      num_vectors,
      bits,
    );
  }
  ScalarQuantizer::from_vectors(
    &float_vector_values,
    confidence_interval.unwrap_or_else(|| {
      Lucene99ScalarQuantizedVectorsFormat::calculate_default_confidence_interval(
        float_vector_values.dimension(),
      )
    }),
    num_vectors,
    bits,
  )
}

#[derive(Clone, Copy)]
pub(crate) struct FloatVectorWrapper<'a> {
  vector_list: &'a [VectorValueEnum],
}

impl<'a> FloatVectorWrapper<'a> {
  pub(crate) fn new(vector_list: &'a [VectorValueEnum]) -> Self {
    Self { vector_list }
  }
}

impl KnnVectorValues for FloatVectorWrapper<'_> {
  fn dimension(&self) -> usize {
    self.vector_list[0].len()
  }

  fn size(&self) -> usize {
    self.vector_list.len()
  }

  type KnnVectorValues = Self;

  fn copy(&self) -> Result<Self::KnnVectorValues> {
    Ok(*self)
  }

  fn get_encoding(&self) -> VectorEncoding {
    FloatVectorValues::get_encoding(self)
  }

  type Bits<'a, B>
    = BitsImpl1<B>
  where
    B: Bits,
    Self: 'a;

  fn get_accept_ords<'a, B>(&'a self, accept_docs: Option<B>) -> Option<Self::Bits<'a, B>>
  where
    B: Bits,
  {
    self.default_get_accept_ords(accept_docs)
  }

  type DocIndexIterator = DenseDocIndexIterator;

  fn iterator(&self) -> Result<Self::DocIndexIterator> {
    Ok(DenseDocIndexIterator::new(self.vector_list.len() as i32))
  }
}

impl FloatVectorValues for FloatVectorWrapper<'_> {
  fn vector_value(&self, ord: usize) -> Result<Cow<'_, VectorValueEnum>> {
    if ord >= self.vector_list.len() {
      return Err(LuceneError::io(std::io::Error::other(format!(
        "vector ord {} out of bounds",
        ord
      ))));
    }
    Ok(Cow::Borrowed(&self.vector_list[ord]))
  }

  type FloatVectorValues = Self;

  fn float_copy(&self) -> Result<Option<Self::FloatVectorValues>> {
    Ok(Some(*self))
  }

  type VectorScorer = DummyVectorScorer;
}

struct QuantizedByteVectorValueSub<V, DM>
where
  V: QuantizedByteVectorValues,
{
  values: V,
  iterator: <V as KnnVectorValues>::DocIndexIterator,
  doc_map: DM,
}

impl<V, DM> QuantizedByteVectorValueSub<V, DM>
where
  V: QuantizedByteVectorValues,
{
  fn new(doc_map: DM, values: V) -> Result<Self> {
    let iterator = values.iterator()?;
    debug_assert_eq!(iterator.doc_id(), -1);
    Ok(Self {
      values,
      iterator,
      doc_map,
    })
  }

  fn index(&self) -> Result<i32> {
    self.iterator.index()
  }
}

impl<V, DM> SubBase for QuantizedByteVectorValueSub<V, DM>
where
  V: QuantizedByteVectorValues,
  DM: MergeDocMap,
{
  type DocMap = DM;

  fn next_doc(&mut self) -> Result<i32> {
    self.iterator.next_doc()
  }

  fn get_doc_map(&self) -> Result<&Self::DocMap> {
    Ok(&self.doc_map)
  }
}

/// Returns a merged view over all the segment's [`QuantizedByteVectorValues`].
struct MergedQuantizedVectorValues<V, LiveBits>
where
  V: QuantizedByteVectorValues,
  LiveBits: Bits,
{
  state: RefCell<Option<MergedQuantizedVectorValuesState<V, Rc<MergeStateDocMapImpl<LiveBits>>>>>,
  #[allow(clippy::type_complexity)]
  iterator_state:
    RefCell<Weak<RefCell<MergedQuantizedVectorValuesState<V, Rc<MergeStateDocMapImpl<LiveBits>>>>>>,
  size: usize,
  dimension: usize,
}

struct MergedQuantizedVectorValuesState<V, DM>
where
  V: QuantizedByteVectorValues,
  DM: MergeDocMap,
{
  doc_id: i32,
  ord: i32,
  current: Option<usize>,
  doc_id_merger: DocIDMergerEnum<QuantizedByteVectorValueSub<V, DM>>,
}

impl<F, LiveBits>
  MergedQuantizedVectorValues<
    QuantizedFloatVectorValues<FloatVectorValuesEnum2<F, NormalizedFloatVectorValues<F>>>,
    LiveBits,
  >
where
  F: FloatVectorValues,
  LiveBits: Bits,
{
  fn merge_quantized_byte_vector_values<D, CR>(
    field_info: &FieldInfo,
    merge_state: &MergeState<'_, D, CR>,
    scalar_quantizer: ScalarQuantizer,
  ) -> Result<Self>
  where
    CR: CodecReader + LeafReader<Bits = LiveBits>,
    CRKnnVectorReader<CR>: KnnVectorsReader<FloatVectorValues = F>,
  {
    debug_assert!(field_info.has_vector_values());

    let mut subs = Vec::new();
    for i in 0..merge_state.knn_vectors_readers.len() {
      if has_vector_values(&merge_state.field_infos[i], &field_info.name)?
        && let Some(knn_vectors_reader) = merge_state.knn_vectors_readers[i].as_ref()
      {
        debug_assert!(scalar_quantizer.get_bits() > 0);
        let to_quantize = knn_vectors_reader.get_float_vector_values(&field_info.name)?;
        let to_quantize =
          if *field_info.get_vector_similarity_function() == VectorSimilarityFunction::Cosine {
            FloatVectorValuesEnum2::B(NormalizedFloatVectorValues::new(to_quantize))
          } else {
            FloatVectorValuesEnum2::A(to_quantize)
          };
        let sub = QuantizedByteVectorValueSub::new(
          merge_state.doc_maps[i].clone(),
          QuantizedFloatVectorValues::new(
            to_quantize,
            *field_info.get_vector_similarity_function(),
            scalar_quantizer.clone(),
          ),
        )?;
        subs.push(Sub::new(sub));
      }
    }
    Self::new(subs, merge_state)
  }
}

impl<V, LiveBits> MergedQuantizedVectorValues<V, LiveBits>
where
  V: QuantizedByteVectorValues,
  LiveBits: Bits,
{
  fn new<D, CR>(
    subs: Vec<Sub<QuantizedByteVectorValueSub<V, Rc<MergeStateDocMapImpl<LiveBits>>>>>,
    merge_state: &MergeState<'_, D, CR>,
  ) -> Result<Self>
  where
    CR: CodecReader,
  {
    let dimension = match subs.first() {
      Some(sub) => sub.sub.values.dimension(),
      None => return Err(LuceneError::illegal_state("no sub-vectors to merge")),
    };
    let size = subs.iter().map(|sub| sub.sub.values.size()).sum();
    let doc_id_merger = of(subs, merge_state.needs_index_sort)?;
    Ok(Self {
      state: RefCell::new(Some(MergedQuantizedVectorValuesState {
        doc_id: -1,
        ord: -1,
        current: None,
        doc_id_merger,
      })),
      iterator_state: RefCell::new(Weak::new()),
      size,
      dimension,
    })
  }
}

impl<V, LiveBits> HasIndexSlice for MergedQuantizedVectorValues<V, LiveBits>
where
  V: QuantizedByteVectorValues,
  LiveBits: Bits,
{
}

impl<V, LiveBits> KnnVectorValues for MergedQuantizedVectorValues<V, LiveBits>
where
  V: QuantizedByteVectorValues,
  LiveBits: Bits,
{
  fn dimension(&self) -> usize {
    self.dimension
  }

  fn size(&self) -> usize {
    self.size
  }

  type KnnVectorValues = Self;

  fn get_encoding(&self) -> VectorEncoding {
    ByteVectorValues::get_encoding(self)
  }

  type Bits<'a, B>
    = BitsImpl1<B>
  where
    B: Bits,
    Self: 'a;

  fn get_accept_ords<'a, B>(&'a self, accept_docs: Option<B>) -> Option<Self::Bits<'a, B>>
  where
    B: Bits,
  {
    self.default_get_accept_ords(accept_docs)
  }

  type DocIndexIterator = MergedQuantizedVectorValuesIterator<V, LiveBits>;

  fn iterator(&self) -> Result<Self::DocIndexIterator> {
    let state = self.state.borrow_mut().take().ok_or_else(|| {
      LuceneError::illegal_state(
        "iterator() can only be called once on MergedQuantizedVectorValues",
      )
    })?;
    let state = Rc::new(RefCell::new(state));
    *self.iterator_state.borrow_mut() = Rc::downgrade(&state);
    Ok(MergedQuantizedVectorValuesIterator {
      state,
      size: self.size,
    })
  }
}

impl<V, LiveBits> ByteVectorValues for MergedQuantizedVectorValues<V, LiveBits>
where
  V: QuantizedByteVectorValues,
  LiveBits: Bits,
{
  fn vector_value(&self, _ord: usize) -> Result<Cow<'_, VectorValueEnum>> {
    let state = self
      .iterator_state
      .borrow()
      .upgrade()
      .ok_or_else(|| LuceneError::illegal_state("missing merged quantized vector iterator"))?;
    let state = state.borrow();
    let current = state
      .current
      .ok_or_else(|| LuceneError::illegal_state("missing current vector sub"))?;
    let current_sub = &state.doc_id_merger.get_subs()[current].sub;
    let index: usize = current_sub.index()?.try_convert()?;
    Ok(Cow::Owned(
      current_sub.values.vector_value(index)?.into_owned(),
    ))
  }

  type ByteVectorValues = DummyByteVectorValues;

  fn byte_copy(&self) -> Result<Option<Self::ByteVectorValues>> {
    Err(LuceneError::unsupported_operation(""))
  }

  type VectorScorer = DummyVectorScorer;
}

impl<V, LiveBits> QuantizedByteVectorValues for MergedQuantizedVectorValues<V, LiveBits>
where
  V: QuantizedByteVectorValues,
  LiveBits: Bits,
{
  fn get_score_correction_constant(&self, _ord: usize) -> Result<f32> {
    let state = self
      .iterator_state
      .borrow()
      .upgrade()
      .ok_or_else(|| LuceneError::illegal_state("missing merged quantized vector iterator"))?;
    let state = state.borrow();
    let current = state
      .current
      .ok_or_else(|| LuceneError::illegal_state("missing current vector sub"))?;
    let current_sub = &state.doc_id_merger.get_subs()[current].sub;
    let index: usize = current_sub.index()?.try_convert()?;
    current_sub.values.get_score_correction_constant(index)
  }

  type QuantizedVectorScorer = DummyVectorScorer;

  fn scorer(&self, _query: &[f32]) -> Result<Option<Self::QuantizedVectorScorer>> {
    Err(LuceneError::unsupported_operation(""))
  }

  type QuantizedByteVectorValues = Self;

  fn copy(&self) -> Result<Self::QuantizedByteVectorValues> {
    Err(LuceneError::unsupported_operation(""))
  }
}

struct MergedQuantizedVectorValuesIterator<V, B>
where
  V: QuantizedByteVectorValues,
  B: Bits,
{
  state: Rc<RefCell<MergedQuantizedVectorValuesState<V, Rc<MergeStateDocMapImpl<B>>>>>,
  size: usize,
}

impl<V, B> DocIdSetIterator for MergedQuantizedVectorValuesIterator<V, B>
where
  V: QuantizedByteVectorValues,
  B: Bits,
{
  fn doc_id(&self) -> i32 {
    self.state.borrow().doc_id
  }

  fn next_doc(&mut self) -> Result<i32> {
    let mut state = self.state.borrow_mut();
    state.current = state.doc_id_merger.next()?;
    match state.current {
      Some(current) => {
        state.doc_id = state.doc_id_merger.get_subs()[current].mapped_doc_id;
        state.ord += 1;
        Ok(state.doc_id)
      },
      None => {
        state.doc_id = NO_MORE_DOCS;
        state.ord = NO_MORE_DOCS;
        Ok(NO_MORE_DOCS)
      },
    }
  }

  fn advance(&mut self, _target: i32) -> Result<i32> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn cost(&self) -> Result<i64> {
    self.size.try_convert()
  }
}

impl<V, B> DocIndexIterator for MergedQuantizedVectorValuesIterator<V, B>
where
  V: QuantizedByteVectorValues,
  B: Bits,
{
  fn index(&self) -> Result<i32> {
    Ok(self.state.borrow().ord)
  }
}

struct QuantizedFloatVectorValues<FVV> {
  values: FVV,
  quantizer: ScalarQuantizer,
  inner: RefCell<QuantizedFloatVectorValuesInner>,
  vector_similarity_function: VectorSimilarityFunction,
}

struct QuantizedFloatVectorValuesInner {
  quantized_vector: Vec<u8>,
  last_ord: i32,
  offset_value: f32,
}

impl<FVV> QuantizedFloatVectorValues<FVV>
where
  FVV: FloatVectorValues,
{
  fn new(
    values: FVV,
    vector_similarity_function: VectorSimilarityFunction,
    quantizer: ScalarQuantizer,
  ) -> Self {
    let quantized_vector = vec![0; values.dimension()];
    Self {
      values,
      quantizer,
      inner: RefCell::new(QuantizedFloatVectorValuesInner {
        quantized_vector,
        last_ord: -1,
        offset_value: 0.0,
      }),
      vector_similarity_function,
    }
  }

  fn quantize(&self, ord: usize, inner: &mut QuantizedFloatVectorValuesInner) -> Result<f32> {
    let vector = self.values.vector_value(ord)?;
    Ok(self.quantizer.quantize(
      vector.as_floats()?,
      &mut inner.quantized_vector,
      self.vector_similarity_function,
    ))
  }
}

impl<FVV> HasIndexSlice for QuantizedFloatVectorValues<FVV> where FVV: FloatVectorValues {}

impl<FVV> KnnVectorValues for QuantizedFloatVectorValues<FVV>
where
  FVV: FloatVectorValues,
{
  fn dimension(&self) -> usize {
    self.values.dimension()
  }

  fn size(&self) -> usize {
    self.values.size()
  }

  fn ord_to_doc(&self, ord: usize) -> Result<usize> {
    self.values.ord_to_doc(ord)
  }

  type KnnVectorValues = Self;

  fn get_encoding(&self) -> VectorEncoding {
    ByteVectorValues::get_encoding(self)
  }

  type Bits<'a, B>
    = FVV::Bits<'a, B>
  where
    B: Bits,
    Self: 'a;

  fn get_accept_ords<'a, B>(&'a self, accept_docs: Option<B>) -> Option<Self::Bits<'a, B>>
  where
    B: Bits,
  {
    self.values.get_accept_ords(accept_docs)
  }

  type DocIndexIterator = FVV::DocIndexIterator;

  fn iterator(&self) -> Result<Self::DocIndexIterator> {
    self.values.iterator()
  }
}

impl<FVV> ByteVectorValues for QuantizedFloatVectorValues<FVV>
where
  FVV: FloatVectorValues,
{
  fn vector_value(&self, ord: usize) -> Result<Cow<'_, VectorValueEnum>> {
    let mut inner = self.inner.borrow_mut();
    let ord_i32: i32 = ord.try_convert()?;
    if ord_i32 != inner.last_ord {
      inner.offset_value = self.quantize(ord, &mut inner)?;
      inner.last_ord = ord_i32;
    }
    Ok(Cow::Owned(VectorValueEnum::Byte(
      inner.quantized_vector.clone(),
    )))
  }

  type ByteVectorValues = Self;

  fn byte_copy(&self) -> Result<Option<Self::ByteVectorValues>> {
    Err(LuceneError::unsupported_operation(""))
  }

  type VectorScorer = DummyVectorScorer;
}

impl<FVV> QuantizedByteVectorValues for QuantizedFloatVectorValues<FVV>
where
  FVV: FloatVectorValues,
{
  fn get_scalar_quantizer(&self) -> Result<ScalarQuantizer> {
    Ok(self.quantizer.clone())
  }

  fn get_score_correction_constant(&self, ord: usize) -> Result<f32> {
    let inner = self.inner.borrow();
    let ord: i32 = ord.try_convert()?;
    if ord != inner.last_ord {
      return Err(LuceneError::illegal_state(format!(
        "attempt to retrieve score correction for different ord {} than the quantization was done for: {}",
        ord, inner.last_ord
      )));
    }
    Ok(inner.offset_value)
  }

  type QuantizedVectorScorer = DummyVectorScorer;

  fn scorer(&self, _query: &[f32]) -> Result<Option<Self::QuantizedVectorScorer>> {
    Err(LuceneError::unsupported_operation(""))
  }

  type QuantizedByteVectorValues = Self;

  fn copy(&self) -> Result<Self::QuantizedByteVectorValues> {
    Err(LuceneError::unsupported_operation(""))
  }
}

struct OffsetCorrectedQuantizedByteVectorValues<Q> {
  in_: Q,
  vector_similarity_function: VectorSimilarityFunction,
  scalar_quantizer: ScalarQuantizer,
  old_scalar_quantizer: ScalarQuantizer,
}

impl<Q> OffsetCorrectedQuantizedByteVectorValues<Q> {
  fn new(
    in_: Q,
    vector_similarity_function: VectorSimilarityFunction,
    scalar_quantizer: ScalarQuantizer,
    old_scalar_quantizer: ScalarQuantizer,
  ) -> Self {
    Self {
      in_,
      vector_similarity_function,
      scalar_quantizer,
      old_scalar_quantizer,
    }
  }
}

impl<Q> HasIndexSlice for OffsetCorrectedQuantizedByteVectorValues<Q>
where
  Q: QuantizedByteVectorValues<QuantizedByteVectorValues = Q>,
{
  fn seek(&self, pos: usize) -> Result<()> {
    self.in_.seek(pos)
  }

  fn read_bytes(&self, b: &mut [u8], offset: usize, len: usize) -> Result<()> {
    self.in_.read_bytes(b, offset, len)
  }
}

impl<Q> KnnVectorValues for OffsetCorrectedQuantizedByteVectorValues<Q>
where
  Q: QuantizedByteVectorValues<QuantizedByteVectorValues = Q>,
{
  fn dimension(&self) -> usize {
    self.in_.dimension()
  }

  fn size(&self) -> usize {
    self.in_.size()
  }

  fn ord_to_doc(&self, ord: usize) -> Result<usize> {
    self.in_.ord_to_doc(ord)
  }

  type KnnVectorValues = Self;

  fn get_encoding(&self) -> VectorEncoding {
    ByteVectorValues::get_encoding(self)
  }

  type Bits<'a, B>
    = Q::Bits<'a, B>
  where
    B: Bits,
    Self: 'a;

  fn get_accept_ords<'a, B>(&'a self, accept_docs: Option<B>) -> Option<Self::Bits<'a, B>>
  where
    B: Bits,
  {
    self.in_.get_accept_ords(accept_docs)
  }

  type DocIndexIterator = Q::DocIndexIterator;

  fn iterator(&self) -> Result<Self::DocIndexIterator> {
    self.in_.iterator()
  }
}

impl<Q> ByteVectorValues for OffsetCorrectedQuantizedByteVectorValues<Q>
where
  Q: QuantizedByteVectorValues<QuantizedByteVectorValues = Q>,
{
  fn vector_value(&self, ord: usize) -> Result<Cow<'_, VectorValueEnum>> {
    self.in_.vector_value(ord)
  }

  type ByteVectorValues = Self;

  fn byte_copy(&self) -> Result<Option<Self::ByteVectorValues>> {
    QuantizedByteVectorValues::copy(self).map(Some)
  }

  type VectorScorer = DummyVectorScorer;
}

impl<Q> QuantizedByteVectorValues for OffsetCorrectedQuantizedByteVectorValues<Q>
where
  Q: QuantizedByteVectorValues<QuantizedByteVectorValues = Q>,
{
  fn get_scalar_quantizer(&self) -> Result<ScalarQuantizer> {
    Ok(self.scalar_quantizer.clone())
  }

  fn get_score_correction_constant(&self, ord: usize) -> Result<f32> {
    let vector = self.in_.vector_value(ord)?;
    Ok(self.scalar_quantizer.recalculate_corrective_offset(
      vector.as_bytes()?,
      &self.old_scalar_quantizer,
      self.vector_similarity_function,
    ))
  }

  type QuantizedVectorScorer = DummyVectorScorer;

  fn scorer(&self, _query: &[f32]) -> Result<Option<Self::QuantizedVectorScorer>> {
    Err(LuceneError::unsupported_operation(""))
  }

  type QuantizedByteVectorValues = Self;

  fn copy(&self) -> Result<Self::QuantizedByteVectorValues> {
    Ok(Self::new(
      QuantizedByteVectorValues::copy(&self.in_)?,
      self.vector_similarity_function,
      self.scalar_quantizer.clone(),
      self.old_scalar_quantizer.clone(),
    ))
  }
}

struct NormalizedFloatVectorValues<FVV> {
  values: FVV,
}

impl<FVV> NormalizedFloatVectorValues<FVV> {
  fn new(values: FVV) -> Self {
    Self { values }
  }
}

impl<FVV> KnnVectorValues for NormalizedFloatVectorValues<FVV>
where
  FVV: FloatVectorValues,
{
  fn dimension(&self) -> usize {
    self.values.dimension()
  }

  fn size(&self) -> usize {
    self.values.size()
  }

  fn ord_to_doc(&self, ord: usize) -> Result<usize> {
    self.values.ord_to_doc(ord)
  }

  type KnnVectorValues = NormalizedFloatVectorValues<FVV::FloatVectorValues>;

  fn copy(&self) -> Result<Self::KnnVectorValues> {
    self
      .values
      .float_copy()?
      .map(NormalizedFloatVectorValues::new)
      .ok_or_else(|| LuceneError::unsupported_operation(""))
  }

  fn get_encoding(&self) -> VectorEncoding {
    KnnVectorValues::get_encoding(&self.values)
  }

  type Bits<'a, B>
    = FVV::Bits<'a, B>
  where
    B: Bits,
    Self: 'a;

  fn get_accept_ords<'a, B>(&'a self, accept_docs: Option<B>) -> Option<Self::Bits<'a, B>>
  where
    B: Bits,
  {
    self.values.get_accept_ords(accept_docs)
  }

  type DocIndexIterator = FVV::DocIndexIterator;

  fn iterator(&self) -> Result<Self::DocIndexIterator> {
    self.values.iterator()
  }
}

impl<FVV> FloatVectorValues for NormalizedFloatVectorValues<FVV>
where
  FVV: FloatVectorValues,
{
  fn vector_value(&self, ord: usize) -> Result<Cow<'_, VectorValueEnum>> {
    let vector_value = self.values.vector_value(ord)?;
    let mut normalized_vector = vector_value.as_floats()?.to_vec();
    VectorUtil::l2normalize(&mut normalized_vector)?;
    Ok(Cow::Owned(VectorValueEnum::Float(normalized_vector)))
  }

  type FloatVectorValues = NormalizedFloatVectorValues<FVV::FloatVectorValues>;

  fn float_copy(&self) -> Result<Option<Self::FloatVectorValues>> {
    self
      .values
      .float_copy()
      .map(|values| values.map(NormalizedFloatVectorValues::new))
  }

  type VectorScorer = DummyVectorScorer;
}

pub struct ScalarQuantizedCloseableRandomVectorScorerSupplier<'a, Q, D, I>
where
  Q: RandomVectorScorerSupplier,
  D: Directory,
  I: IndexInput,
{
  supplier: Q,
  num_vectors: i32,
  dir: &'a D,
  temp_file: String,
  quantization_data_input: I,
  closed: bool,
}

impl<'a, Q, D, I> ScalarQuantizedCloseableRandomVectorScorerSupplier<'a, Q, D, I>
where
  Q: RandomVectorScorerSupplier,
  D: Directory,
  I: IndexInput,
{
  fn new_quantized(
    num_vectors: i32,
    supplier: Q,
    dir: &'a D,
    temp_file: String,
    quantization_data_input: I,
  ) -> Self {
    Self {
      supplier,
      num_vectors,
      dir,
      temp_file,
      quantization_data_input,
      closed: false,
    }
  }
}

impl<Q, D, I> RandomVectorScorerSupplier
  for ScalarQuantizedCloseableRandomVectorScorerSupplier<'_, Q, D, I>
where
  Q: RandomVectorScorerSupplier,
  D: Directory,
  I: IndexInput,
{
  type Scorer<'a>
    = Q::Scorer<'a>
  where
    Self: 'a;

  fn scorer(&self, ord: usize) -> Result<Self::Scorer<'_>> {
    self.supplier.scorer(ord)
  }

  type RandomVectorScorerSupplier = Q::RandomVectorScorerSupplier;

  fn copy(&self) -> Result<Self::RandomVectorScorerSupplier>
  where
    Self: Sized,
  {
    self.supplier.copy()
  }

  fn get_vector(&self) -> Result<&[VectorValueEnum]> {
    self.supplier.get_vector()
  }

  fn get_vector_mut(&mut self) -> Result<&mut Vec<VectorValueEnum>> {
    self.supplier.get_vector_mut()
  }

  fn ram_bytes_used(&self) -> Result<i64> {
    self.supplier.ram_bytes_used()
  }
}

impl<Q, D, I> Closeable for ScalarQuantizedCloseableRandomVectorScorerSupplier<'_, Q, D, I>
where
  Q: RandomVectorScorerSupplier,
  D: Directory,
  I: IndexInput,
{
  fn close(&mut self) -> Result<()> {
    if !self.closed {
      self.closed = true;
      CloseableRef::close(&self.quantization_data_input)?;
      self.dir.delete_file(&self.temp_file)
    } else {
      Ok(())
    }
  }
}

impl<Q, D, I> CloseableRandomVectorScorerSupplier
  for ScalarQuantizedCloseableRandomVectorScorerSupplier<'_, Q, D, I>
where
  Q: RandomVectorScorerSupplier,
  D: Directory,
  I: IndexInput,
{
  fn total_vector_count(&self) -> Result<i32> {
    Ok(self.num_vectors)
  }
}

impl<Q, D, I> Drop for ScalarQuantizedCloseableRandomVectorScorerSupplier<'_, Q, D, I>
where
  Q: RandomVectorScorerSupplier,
  D: Directory,
  I: IndexInput,
{
  fn drop(&mut self) {
    let _ = self.close();
  }
}
