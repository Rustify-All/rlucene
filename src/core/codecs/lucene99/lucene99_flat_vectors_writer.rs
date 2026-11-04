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
use crate::core::codecs::knn_field_vectors_writer::KnnFieldVectorsWriter;
use crate::core::codecs::knn_field_vectors_writer::VectorInput;
use crate::core::codecs::knn_vectors_writer::{
  KnnVectorsWriter, MergedByteVectorValues, MergedFloat32VectorValues,
  map_old_ord_to_new_ord, merge_byte_vector_values, merge_float_vector_values,
};
use crate::core::codecs::lucene95::off_heap_byte_vector_values;
use crate::core::codecs::lucene95::off_heap_float_vector_values;
use crate::core::codecs::lucene95::ord_to_doc_disi_reader_configuration::OrdToDocDISIReaderConfiguration;
use crate::core::codecs::lucene99::lucene99_flat_vectors_format::DIRECT_MONOTONIC_BLOCK_SHIFT;
use crate::core::codecs::lucene99::lucene99_flat_vectors_format::{
  META_CODEC_NAME, META_EXTENSION, VECTOR_DATA_CODEC_NAME, VECTOR_DATA_EXTENSION, VERSION_CURRENT,
};
use crate::core::codecs::lucene99::lucene99_hnsw_vectors_writer::EncodedFieldWriter;
use crate::core::index::IndexFileNames;
use crate::core::index::byte_vector_values::ByteVectorValues;
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::docs_with_field_set::DocsWithFieldSet;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::float_vector_values::FloatVectorValues;
use crate::core::index::knn_vector_values::{DocIndexIterator, KnnVectorValues};
use crate::core::index::merge_state::{DocMap as MergeDocMap, MergeState};
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::sorter::DocMap;
use crate::core::index::vector_encoding::VectorEncoding;
use crate::core::index::vector_similarity_function::VectorSimilarity;
use crate::core::search::doc_id_set::DocIdSet;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::store::directory::Directory;
use crate::core::store::index_input::IndexInput;
use crate::core::store::{IndexOutput, ReadAdvice};
use crate::core::util::TryIntoInt;
use crate::core::util::accountable::Accountable;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::clone::TryClone;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::hnsw::closeable_random_vector_scorer_supplier::CloseableRandomVectorScorerSupplier;
use crate::core::util::hnsw::random_vector_scorer_supplier::RandomVectorScorerSupplier;
use crate::core::util::io_utils::IOUtils;
use crate::core::util::ram_usage_estimator::size_of_vec;
use std::sync::Arc;

/// Writes vector values to index segments.
pub struct Lucene99FlatVectorsWriter<O, F> {
  meta: O,
  vector_data: O,
  fields: Vec<FlatFieldWriter>,
  finished: bool,
  flat_vectors_scorer: F,
}
impl<O, F> Lucene99FlatVectorsWriter<O, F>
where
  O: IndexOutput,
  F: FlatVectorsScorer,
{
  pub fn new<D1, D2>(
    state: &SegmentWriteState<D1>,
    scorer: F,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self>
  where
    D1: Directory<IndexOutput = O>,
  {
    let meta_file_name =
      IndexFileNames::segment_file_name(&segment_info.name, &state.segment_suffix, META_EXTENSION);

    let vector_data_file_name = IndexFileNames::segment_file_name(
      &segment_info.name,
      &state.segment_suffix,
      VECTOR_DATA_EXTENSION,
    );

    let mut meta = None;
    let mut vector_data = None;
    let mut success = false;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      meta = Some(
        state
          .directory
          .create_output(&meta_file_name, state.context)?,
      );
      vector_data = Some(
        state
          .directory
          .create_output(&vector_data_file_name, state.context)?,
      );

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
        vector_data
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("vector data output is missing"))?,
        VECTOR_DATA_CODEC_NAME,
        VERSION_CURRENT,
        segment_info.get_id(),
        &state.segment_suffix,
      )?;
      success = true;
      Ok(())
    }));

    if !success {
      IOUtils::close_while_handling_exception((meta.as_mut(), vector_data.as_mut()));
    }
    unwrap_caught_result!(result)?;
    let (meta, vector_data) = match (meta, vector_data) {
      (Some(meta), Some(vector_data)) => (meta, vector_data),
      (mut meta, mut vector_data) => {
        IOUtils::close_while_handling_exception((meta.as_mut(), vector_data.as_mut()));
        return Err(LuceneError::illegal_state(
          "flat vector outputs are missing after successful construction",
        ));
      },
    };

    Ok(Self {
      meta,
      vector_data,
      fields: Vec::new(),
      finished: false,
      flat_vectors_scorer: scorer,
    })
  }

  fn write_float32_vectors(vector_data: &mut O, dim: usize, vectors: &[Box<[f32]>]) -> Result<()> {
    #[cfg(target_endian = "little")]
    if dim >= 16 {
      for vector in vectors {
        debug_assert_eq!(vector.len(), dim);
        let bytes: &[u8] = wide::bytemuck::cast_slice(vector);
        vector_data.write_bytes_range(bytes, 0, bytes.len())?;
      }
      return Ok(());
    }

    let byte_size = BitUtil::FLOAT_BYTES;
    stack_or_heap_buffer!(buffer, u8, dim * byte_size, 128, 0);

    for vector in vectors.iter() {
      debug_assert_eq!(vector.len(), dim);
      write_float_vector(vector, buffer)?;
      vector_data.write_bytes_range(buffer, 0, buffer.len())?;
    }

    Ok(())
  }

  fn write_byte_vectors(vector_data: &mut O, vectors: &[Box<[u8]>]) -> Result<()> {
    for vector in vectors.iter() {
      vector_data.write_bytes_range(vector, 0, vector.len())?;
    }

    Ok(())
  }
  fn write_sorted_float32_vectors(
    vector_data: &mut O,
    field_data: &FlatFieldWriter,
    ord_map: &[usize],
    vectors: &[Box<[f32]>],
  ) -> Result<usize>
  where
    O: IndexOutput,
  {
    let vector_data_offset = vector_data.align_file_pointer(BitUtil::FLOAT_BYTES)?;

    let dim = field_data.dim;
    #[cfg(target_endian = "little")]
    if dim >= 16 {
      for &ord in ord_map {
        let vector = &vectors[ord];
        debug_assert_eq!(vector.len(), dim);
        let bytes: &[u8] = wide::bytemuck::cast_slice(vector);
        vector_data.write_bytes_range(bytes, 0, bytes.len())?;
      }
      return Ok(vector_data_offset);
    }

    let byte_size = BitUtil::FLOAT_BYTES;
    stack_or_heap_buffer!(buffer, u8, dim * byte_size, 128, 0);

    for &ord in ord_map {
      let vector = &vectors[ord];
      debug_assert_eq!(vector.len(), dim);
      write_float_vector(vector, buffer)?;
      vector_data.write_bytes_range(buffer, 0, buffer.len())?;
    }

    Ok(vector_data_offset)
  }
  fn write_sorted_byte_vectors(
    vector_data: &mut O,
    ord_map: &[usize],
    vectors: &[Box<[u8]>],
  ) -> Result<usize>
  where
    O: IndexOutput,
  {
    let vector_data_offset = vector_data.align_file_pointer(BitUtil::FLOAT_BYTES)?;
    for &ord in ord_map {
      let vector = &vectors[ord];
      vector_data.write_bytes_range(vector, 0, vector.len())?;
    }
    Ok(vector_data_offset)
  }
  fn write_field(
    meta: &mut O,
    vector_data: &mut O,
    fields: &mut [FlatFieldWriter],
    field_data_idx: usize,
    max_doc: i32,
    vectors: Option<VectorSlices<'_>>,
  ) -> Result<()>
  where
    O: IndexOutput,
  {
    let field_data = fields.get_mut(field_data_idx).ok_or_else(|| {
      LuceneError::illegal_argument(format!("Invalid field_data_idx: {}", field_data_idx))
    })?;
    field_data.docs_with_field.finish();
    let vectors = vectors.unwrap_or_else(|| field_data.vectors.as_slices());
    let vector_data_offset = vector_data.align_file_pointer(BitUtil::FLOAT_BYTES)?;

    match vectors {
      VectorSlices::Byte(vectors) => {
        Self::write_byte_vectors(vector_data, vectors)?;
      },
      VectorSlices::Float(vectors) => {
        Self::write_float32_vectors(vector_data, field_data.dim, vectors)?;
      },
    }

    let vector_data_length = vector_data.get_file_pointer()? - vector_data_offset;

    write_meta(
      meta,
      vector_data,
      &field_data.field_info,
      max_doc,
      vector_data_offset,
      vector_data_length,
      field_data.get_docs_with_field_set(),
    )?;

    Ok(())
  }
  fn write_sorting_field<DM>(
    meta: &mut O,
    vector_data: &mut O,
    fields: &mut [FlatFieldWriter],
    field_data_idx: usize,
    max_doc: i32,
    sort_map: &DM,
    vectors: Option<VectorSlices<'_>>,
  ) -> Result<()>
  where
    DM: DocMap,
  {
    let field_data = fields.get_mut(field_data_idx).ok_or_else(|| {
      LuceneError::illegal_argument(format!("Invalid field_data_idx: {}", field_data_idx))
    })?;
    field_data.docs_with_field.finish();
    let vectors = vectors.unwrap_or_else(|| field_data.vectors.as_slices());

    let cardinality = field_data.get_docs_with_field_set().cardinality() as usize;

    // new ord -> old ord
    let mut ord_map = vec![0usize; cardinality];

    let mut new_docs_with_field = DocsWithFieldSet::new();

    map_old_ord_to_new_ord(
      field_data.get_docs_with_field_set(),
      sort_map,
      None,
      Some(&mut ord_map),
      Some(&mut new_docs_with_field),
    )?;
    new_docs_with_field.finish();

    // write vector values
    let vector_data_offset = match vectors {
      VectorSlices::Byte(vectors) => {
        Self::write_sorted_byte_vectors(vector_data, &ord_map, vectors)?
      },
      VectorSlices::Float(vectors) => {
        Self::write_sorted_float32_vectors(vector_data, field_data, &ord_map, vectors)?
      },
    };
    let vector_data_length = vector_data.get_file_pointer()? - vector_data_offset;

    write_meta(
      meta,
      vector_data,
      &field_data.field_info,
      max_doc,
      vector_data_offset,
      vector_data_length,
      &new_docs_with_field,
    )?;

    Ok(())
  }
}
impl<O, F> Accountable for Lucene99FlatVectorsWriter<O, F> {
  fn ram_bytes_used(&self) -> Result<i64> {
    let mut size = size_of_vec(&self.fields);
    for field in &self.fields {
      size = size.saturating_add(field.ram_bytes_used()?);
    }
    Ok(size)
  }
}

impl<O, F> Closeable for Lucene99FlatVectorsWriter<O, F>
where
  O: Closeable,
{
  fn close(&mut self) -> Result<()> {
    IOUtils::close([&mut self.meta, &mut self.vector_data])
  }
}

impl<O, F> KnnVectorsWriter<O> for Lucene99FlatVectorsWriter<O, F>
where
  O: IndexOutput,
  F: FlatVectorsScorer,
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
    let field_idx = self.fields.len();
    self.fields.push(FlatFieldWriter::new(field_info));
    Ok(field_idx)
  }

  fn flush<DM>(&mut self, max_doc: i32, sort_map: Option<&DM>) -> Result<()>
  where
    DM: DocMap,
  {
    for field_idx in 0..self.fields.len() {
      if let Some(sort_map) = sort_map {
        Self::write_sorting_field(
          &mut self.meta,
          &mut self.vector_data,
          &mut self.fields,
          field_idx,
          max_doc,
          sort_map,
          None,
        )?;
      } else {
        Self::write_field(
          &mut self.meta,
          &mut self.vector_data,
          &mut self.fields,
          field_idx,
          max_doc,
          None,
        )?;
      }
      self.fields[field_idx].finish()?;
    }
    Ok(())
  }

  fn merge_one_field<D1, D2, CR>(
    &mut self,
    field_info: &Arc<FieldInfo>,
    merge_state: &MergeState<'_, D1, CR>,
    _segment_write_state: &SegmentWriteState<&D2>,
  ) -> Result<()>
  where
    D2: Directory<IndexOutput = O>,
    CR: CodecReader,
    Self: Sized,
  {
    let vector_data_offset = self.vector_data.align_file_pointer(BitUtil::FLOAT_BYTES)?;
    let docs_with_field = match field_info.get_vector_encoding() {
      VectorEncoding::BYTE(_) => {
        let mut merged_bytes = merge_byte_vector_values(field_info.as_ref(), merge_state)?;
        write_byte_vector_data(&mut self.vector_data, &mut merged_bytes)?
      },
      VectorEncoding::FLOAT32(_) => {
        let mut merged_floats = merge_float_vector_values(field_info.as_ref(), merge_state)?;
        write_vector_data(&mut self.vector_data, &mut merged_floats)?
      },
    };
    let vector_data_length = self.vector_data.get_file_pointer()? - vector_data_offset;
    write_meta(
      &mut self.meta,
      &mut self.vector_data,
      field_info.as_ref(),
      merge_state.segment_info.max_doc()?,
      vector_data_offset,
      vector_data_length,
      &docs_with_field,
    )?;
    Ok(())
  }

  fn finish(&mut self) -> Result<()> {
    if self.finished {
      return Err(LuceneError::illegal_state("already finished"));
    }
    self.finished = true;

    // write end of fields marker
    self.meta.write_int(-1)?;
    CodecUtil::write_footer(&mut self.meta)?;

    CodecUtil::write_footer(&mut self.vector_data)?;

    Ok(())
  }

  fn add_value<V: VectorInput + ?Sized>(
    &mut self,
    doc_id: i32,
    vector_value: &V,
    field_vectors_writers_idx: usize,
  ) -> Result<()> {
    let field = self
      .fields
      .get_mut(field_vectors_writers_idx)
      .ok_or_else(|| {
        LuceneError::illegal_state(format!(
          "Invalid field vectors writer index: {field_vectors_writers_idx}"
        ))
      })?;
    if field.finished {
      return Err(LuceneError::illegal_state(
        "already finished, cannot add more values",
      ));
    }
    if doc_id == field.last_doc_id {
      return Err(LuceneError::illegal_argument(format!(
        "VectorValuesField \"{}\" appears more than once in this document (only one value is allowed per field)",
        field.field_info.name
      )));
    }
    debug_assert!(doc_id > field.last_doc_id);

    match &mut field.vectors {
      VectorBuffers::Byte(vectors) => append_vector(
        vectors,
        vector_value,
        field.dim,
        &mut field.docs_with_field,
        &mut field.vector_buffer_bytes,
        doc_id,
      )?,
      VectorBuffers::Float(vectors) => append_vector(
        vectors,
        vector_value,
        field.dim,
        &mut field.docs_with_field,
        &mut field.vector_buffer_bytes,
        doc_id,
      )?,
    }
    field.last_doc_id = doc_id;
    Ok(())
  }
}

impl<O, F> FlatVectorsWriter for Lucene99FlatVectorsWriter<O, F>
where
  O: IndexOutput,
  F: FlatVectorsScorer + Clone + Send,
{
  type IndexOutput = O;
  type FlatVectorsScorer = F;

  fn get_flat_vector_scorer(&self) -> &Self::FlatVectorsScorer {
    &self.flat_vectors_scorer
  }

  fn flat_add_field(&mut self, field_info: Arc<FieldInfo>) -> Result<usize> {
    let len = self.fields.len();
    let new_field = FlatFieldWriter::new(field_info);
    self.fields.push(new_field);
    Ok(len)
  }

  fn flat_flush<DM, FS>(
    &mut self,
    max_doc: i32,
    sort_map: Option<&DM>,
    fields: &[EncodedFieldWriter<FS>],
  ) -> Result<()>
  where
    DM: DocMap,
    FS: FlatVectorsScorer,
  {
    for idx in 0..self.fields.len() {
      let fields = &fields[idx];
      let vectors = fields.get_vectors()?;
      if let Some(sm) = sort_map {
        Self::write_sorting_field(
          &mut self.meta,
          &mut self.vector_data,
          &mut self.fields,
          idx,
          max_doc,
          sm,
          Some(vectors),
        )?;
      } else {
        Self::write_field(
          &mut self.meta,
          &mut self.vector_data,
          &mut self.fields,
          idx,
          max_doc,
          Some(vectors),
        )?;
      }
      self.fields[idx].finish()?;
    }
    Ok(())
  }

  type FlatFieldVectorsWriter = FlatFieldWriter;

  fn get_fields_mut(&mut self) -> &mut [Self::FlatFieldVectorsWriter] {
    self.fields.as_mut()
  }

  type ByteMergeScorerSupplier<'a, D, Sim: VectorSimilarity>
    = FlatCloseableRandomVectorScorerSupplier<
    'a,
    <F as FlatVectorsScorer>::ByteScorerSupplier<
      off_heap_byte_vector_values::DenseOffHeapVectorValues<D::IndexInput, F>,
      Sim,
    >,
    D,
  >
  where
    D: Directory,
    Self: 'a,
    D: 'a;

  fn merge_one_field_to_index_byte<'a, D1, D2, CR, Sim: VectorSimilarity>(
    &'a mut self,
    field_info: &FieldInfo,
    merge_state: &MergeState<'_, D1, CR>,
    segment_write_state: &SegmentWriteState<'a, &'a D2>,
    similarity: Sim,
  ) -> Result<Self::ByteMergeScorerSupplier<'a, D2, Sim>>
  where
    D2: Directory<IndexOutput = Self::IndexOutput>,
    CR: CodecReader,
  {
    self.merge_one_field_with_supplier(
      field_info,
      merge_state,
      segment_write_state,
      |scorer, input, count| {
        let dimension = field_info.get_vector_dimension() as usize;
        let values = off_heap_byte_vector_values::DenseOffHeapVectorValues::new(
          dimension,
          count,
          input,
          dimension * VectorEncoding::BYTE(1).byte_size(),
          scorer.clone(),
          *field_info.get_vector_similarity_function(),
        );
        scorer.get_byte_scorer_supplier(similarity, values)
      },
    )
  }
  type FloatMergeScorerSupplier<'a, D, Sim: VectorSimilarity>
    = FlatCloseableRandomVectorScorerSupplier<
    'a,
    <F as FlatVectorsScorer>::FloatScorerSupplier<
      off_heap_float_vector_values::DenseOffHeapVectorValues<D::IndexInput, F>,
      Sim,
    >,
    D,
  >
  where
    D: Directory,
    Self: 'a,
    D: 'a;

  fn merge_one_field_to_index_float<'a, D1, D2, CR, Sim: VectorSimilarity>(
    &'a mut self,
    field_info: &FieldInfo,
    merge_state: &MergeState<'_, D1, CR>,
    segment_write_state: &SegmentWriteState<'a, &'a D2>,
    similarity: Sim,
  ) -> Result<Self::FloatMergeScorerSupplier<'a, D2, Sim>>
  where
    D2: Directory<IndexOutput = Self::IndexOutput>,
    CR: CodecReader,
  {
    self.merge_one_field_with_supplier(
      field_info,
      merge_state,
      segment_write_state,
      |scorer, input, count| {
        let dimension = field_info.get_vector_dimension() as usize;
        let values = off_heap_float_vector_values::DenseOffHeapVectorValues::new(
          dimension,
          count,
          input,
          dimension * VectorEncoding::FLOAT32(4).byte_size(),
          scorer.clone(),
          *field_info.get_vector_similarity_function(),
        )?;
        scorer.get_float_scorer_supplier(similarity, values)
      },
    )
  }
}

impl<O, F> Lucene99FlatVectorsWriter<O, F>
where
  O: IndexOutput,
  F: FlatVectorsScorer + Clone + Send,
{
  fn merge_one_field_with_supplier<'a, D1, D2, CR, SS, Make>(
    &'a mut self,
    field_info: &FieldInfo,
    merge_state: &MergeState<'_, D1, CR>,
    segment_write_state: &SegmentWriteState<'a, &'a D2>,
    make_supplier: Make,
  ) -> Result<FlatCloseableRandomVectorScorerSupplier<'a, SS, D2>>
  where
    D2: Directory<IndexOutput = O>,
    CR: CodecReader,
    SS: RandomVectorScorerSupplier,
    Make: FnOnce(&F, D2::IndexInput, usize) -> Result<SS>,
  {
    let vector_data_offset = self.vector_data.align_file_pointer(BitUtil::FLOAT_BYTES)?;
    let mut temp_vector_data = segment_write_state.directory.create_temp_output(
      self.vector_data.get_name(),
      "temp",
      segment_write_state.context,
    )?;
    let mut temp_vector_name = temp_vector_data.get_name().to_string();
    let mut vector_data_input = None;
    let mut success = false;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
      || -> Result<FlatCloseableRandomVectorScorerSupplier<'_, SS, D2>> {
        let docs_with_field = match field_info.get_vector_encoding() {
          VectorEncoding::BYTE(_) => {
            let mut merged_bytes = merge_byte_vector_values(field_info, merge_state)?;
            write_byte_vector_data(&mut temp_vector_data, &mut merged_bytes)?
          },
          VectorEncoding::FLOAT32(_) => {
            let mut merged_floats = merge_float_vector_values(field_info, merge_state)?;
            write_vector_data(&mut temp_vector_data, &mut merged_floats)?
          },
        };
        CodecUtil::write_footer(&mut temp_vector_data)?;
        IOUtils::close(&mut temp_vector_data)?;

        let random_context = segment_write_state
          .context
          .with_read_advice_self(ReadAdvice::Random)?;
        vector_data_input = Some(
          segment_write_state
            .directory
            .open_input(&temp_vector_name, &random_context)?,
        );
        let vector_data_input_ref = vector_data_input
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("temporary vector data input is missing"))?;
        let copy_len = vector_data_input_ref.length()? - CodecUtil::footer_length();
        self
          .vector_data
          .copy_bytes(vector_data_input_ref, copy_len)?;
        CodecUtil::retrieve_checksum(vector_data_input_ref)?;

        let vector_data_length = self.vector_data.get_file_pointer()? - vector_data_offset;
        write_meta(
          &mut self.meta,
          &mut self.vector_data,
          field_info,
          merge_state.segment_info.max_doc()?,
          vector_data_offset,
          vector_data_length,
          &docs_with_field,
        )?;
        success = true;

        let vector_values_input = vector_data_input_ref.try_clone()?;
        let random_vector_scorer_supplier = make_supplier(
          &self.flat_vectors_scorer,
          vector_values_input,
          docs_with_field.cardinality() as usize,
        )?;
        let vector_data_input = vector_data_input
          .take()
          .ok_or_else(|| LuceneError::illegal_state("temporary vector data input is missing"))?;
        Ok(FlatCloseableRandomVectorScorerSupplier::new(
          docs_with_field.cardinality(),
          random_vector_scorer_supplier,
          segment_write_state.directory,
          std::mem::take(&mut temp_vector_name),
          vector_data_input,
        ))
      },
    ));

    if !success {
      IOUtils::close_while_handling_exception((vector_data_input.as_ref(), &mut temp_vector_data));
      IOUtils::delete_files_ignoring_exceptions(
        segment_write_state.directory,
        std::iter::once(&temp_vector_name),
      );
    }
    unwrap_caught_result!(result)
  }
}

fn write_meta<O>(
  meta: &mut O,
  vector_data: &mut O,
  field: &FieldInfo,
  max_doc: i32,
  vector_data_offset: usize,
  vector_data_length: usize,
  docs_with_field: &DocsWithFieldSet,
) -> Result<()>
where
  O: IndexOutput,
{
  meta.write_int(field.number)?;
  meta.write_int(field.get_vector_encoding().ordinal())?;
  meta.write_int(field.get_vector_similarity_function().ordinal())?;

  meta.write_vlong(vector_data_offset as i64)?;
  meta.write_vlong(vector_data_length as i64)?;
  meta.write_vint(field.get_vector_dimension())?;

  // write docIDs
  let count = docs_with_field.cardinality();
  meta.write_int(count)?;
  OrdToDocDISIReaderConfiguration::write_stored_meta(
    DIRECT_MONOTONIC_BLOCK_SHIFT,
    meta,
    vector_data,
    count,
    max_doc,
    docs_with_field,
  )?;

  Ok(())
}
/// Writes the byte vector values to the output and returns a set of documents that contains vectors.
fn write_byte_vector_data<O, B, DM>(
  output: &mut O,
  byte_vector_values: &mut MergedByteVectorValues<B, DM>,
) -> Result<DocsWithFieldSet>
where
  O: IndexOutput,
  B: ByteVectorValues,
  DM: MergeDocMap,
{
  let mut docs_with_field = DocsWithFieldSet::new();

  let dim = byte_vector_values.dimension() * VectorEncoding::BYTE(1).byte_size();
  let mut iter = byte_vector_values.iterator()?;

  loop {
    let doc = iter.next_doc()?;
    if doc == NO_MORE_DOCS {
      break;
    }
    let ord: usize = iter.index()?.try_convert()?;
    iter.with_vector_value(ord, |value| {
      debug_assert_eq!(value.len(), dim);
      output.write_bytes_range(&value, 0, value.len())
    })?;
    docs_with_field.add(doc)?;
  }
  docs_with_field.finish();
  Ok(docs_with_field)
}
/// Writes the vector values to the output and returns a set of documents that contains vectors.
fn write_vector_data<O, B, DM>(
  output: &mut O,
  float_vector_values: &mut MergedFloat32VectorValues<B, DM>,
) -> Result<DocsWithFieldSet>
where
  O: IndexOutput,
  B: FloatVectorValues,
  DM: MergeDocMap,
{
  let mut docs_with_field = DocsWithFieldSet::new();

  let dim = float_vector_values.dimension();
  #[cfg(target_endian = "big")]
  let byte_size = BitUtil::FLOAT_BYTES;
  #[cfg(target_endian = "big")]
  stack_or_heap_buffer!(buffer, u8, dim * byte_size, 128, 0);

  let mut iter = float_vector_values.iterator()?;
  loop {
    let doc = iter.next_doc()?;
    if doc == NO_MORE_DOCS {
      break;
    }
    let ord: usize = iter.index()?.try_convert()?;
    iter.with_vector_value(ord, |value| {
      #[cfg(target_endian = "little")]
      let bytes: &[u8] = {
        debug_assert_eq!(value.len(), dim);
        wide::bytemuck::cast_slice(&value)
      };
      #[cfg(target_endian = "big")]
      let bytes: &[u8] = {
        for (i, &v) in value.iter().enumerate() {
          let start = i * byte_size;
          buffer[start..start + byte_size].copy_from_slice(&v.to_le_bytes());
        }
        buffer
      };
      output.write_bytes_range(bytes, 0, bytes.len())
    })?;
    docs_with_field.add(doc)?;
  }
  docs_with_field.finish();
  Ok(docs_with_field)
}
pub struct FlatFieldWriter {
  field_info: Arc<FieldInfo>,
  dim: usize,
  docs_with_field: DocsWithFieldSet,
  finished: bool,
  last_doc_id: i32,
  vectors: VectorBuffers,
  vector_buffer_bytes: i64,
}
impl FlatFieldWriter {
  pub fn new(field_info: Arc<FieldInfo>) -> Self {
    let dim = field_info.get_vector_dimension() as usize;
    let vectors = VectorBuffers::new(*field_info.get_vector_encoding());
    Self {
      field_info,
      dim,
      docs_with_field: DocsWithFieldSet::new(),
      finished: false,
      last_doc_id: -1,
      vectors,
      vector_buffer_bytes: 0,
    }
  }
}

impl Accountable for FlatFieldWriter {
  fn ram_bytes_used(&self) -> Result<i64> {
    let size = self.vectors.shallow_buffer_bytes();
    Ok(
      size
        .saturating_add(self.vector_buffer_bytes)
        .saturating_add(self.docs_with_field.ram_bytes_used()?),
    )
  }
}

impl KnnFieldVectorsWriter for FlatFieldWriter {
  fn copy_value<T: VectorElement, V: VectorInput + ?Sized>(
    &self,
    vector_value: &V,
  ) -> Result<Box<[T]>> {
    T::copy_value(vector_value, self.dim)
  }
}
impl FlatFieldVectorsWriter for FlatFieldWriter {
  fn get_vectors(&self) -> Result<VectorBuffers> {
    Ok(self.vectors.clone())
  }

  fn get_docs_with_field_set(&self) -> &DocsWithFieldSet {
    &self.docs_with_field
  }

  fn finish(&mut self) -> Result<()> {
    if self.finished {
      return Ok(());
    }
    self.finished = true;
    Ok(())
  }

  fn is_finished(&self) -> bool {
    self.finished
  }

  fn flat_add_value<T: VectorElement, V: VectorInput + ?Sized>(
    &mut self,
    doc_id: i32,
    vector_value: &V,
    vector: &mut Vec<Box<[T]>>,
  ) -> Result<()> {
    if self.finished {
      return Err(LuceneError::illegal_state(
        "already finished, cannot add more values",
      ));
    }

    if doc_id == self.last_doc_id {
      return Err(LuceneError::illegal_argument(format!(
        "VectorValuesField \"{}\" appears more than once in this document (only one value is allowed per field)",
        self.field_info.name
      )));
    }

    debug_assert!(doc_id > self.last_doc_id);

    let copy = T::copy_value(vector_value, self.dim)?;

    self.docs_with_field.add(doc_id)?;
    vector.push(copy);

    self.last_doc_id = doc_id;

    Ok(())
  }
}

pub struct FlatCloseableRandomVectorScorerSupplier<'a, S, D>
where
  S: RandomVectorScorerSupplier,
  D: Directory,
{
  supplier: S,
  num_vectors: i32,
  dir: &'a D,
  temp_file: String,
  vector_data_input: D::IndexInput,
  closed: bool,
}

impl<'a, S, D> FlatCloseableRandomVectorScorerSupplier<'a, S, D>
where
  S: RandomVectorScorerSupplier,
  D: Directory,
{
  pub(crate) fn new(
    num_vectors: i32,
    supplier: S,
    dir: &'a D,
    temp_file: String,
    vector_data_input: D::IndexInput,
  ) -> Self {
    Self {
      supplier,
      num_vectors,
      dir,
      temp_file,
      vector_data_input,
      closed: false,
    }
  }
}

impl<S, D> RandomVectorScorerSupplier for FlatCloseableRandomVectorScorerSupplier<'_, S, D>
where
  S: RandomVectorScorerSupplier,
  D: Directory,
{
  type Scorer<'a>
    = S::Scorer<'a>
  where
    Self: 'a;

  type RandomVectorScorerSupplier = S::RandomVectorScorerSupplier;

  fn scorer(&self, ord: usize) -> Result<Self::Scorer<'_>> {
    self.supplier.scorer(ord)
  }

  fn copy(&self) -> Result<Self::RandomVectorScorerSupplier>
  where
    Self: Sized,
  {
    self.supplier.copy()
  }

  fn ram_bytes_used(&self) -> Result<i64> {
    self.supplier.ram_bytes_used()
  }
}

impl<S, D> Closeable for FlatCloseableRandomVectorScorerSupplier<'_, S, D>
where
  S: RandomVectorScorerSupplier,
  D: Directory,
{
  fn close(&mut self) -> Result<()> {
    if !self.closed {
      self.closed = true;
      self.vector_data_input.close()?;
      self.dir.delete_file(&self.temp_file)
    } else {
      Ok(())
    }
  }
}

impl<S, D> CloseableRandomVectorScorerSupplier for FlatCloseableRandomVectorScorerSupplier<'_, S, D>
where
  S: RandomVectorScorerSupplier,
  D: Directory,
{
  fn total_vector_count(&self) -> Result<i32> {
    Ok(self.num_vectors)
  }
}
impl<S, D> Drop for FlatCloseableRandomVectorScorerSupplier<'_, S, D>
where
  S: RandomVectorScorerSupplier,
  D: Directory,
{
  fn drop(&mut self) {
    let _ = self.close();
  }
}

use crate::core::codecs::knn_field_vectors_writer::write_float_vector;
use crate::core::index::vector_buffer::{VectorBuffers, VectorElement, VectorSlices};

fn append_vector<T: VectorElement, V: VectorInput + ?Sized>(
  vectors: &mut Arc<Vec<Box<[T]>>>,
  value: &V,
  dimension: usize,
  docs: &mut DocsWithFieldSet,
  vector_buffer_bytes: &mut i64,
  doc_id: i32,
) -> Result<()> {
  let copy = T::copy_value(value, dimension)?;
  let vector_bytes = std::mem::size_of_val(copy.as_ref()) as i64;
  docs.add(doc_id)?;
  Arc::make_mut(vectors).push(copy);
  *vector_buffer_bytes = vector_buffer_bytes.saturating_add(vector_bytes);
  Ok(())
}
