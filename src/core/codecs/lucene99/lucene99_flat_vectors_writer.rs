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
use crate::core::codecs::knn_field_vectors_writer::{KnnFieldVectorsWriter, VectorValueEnum};
use crate::core::codecs::knn_vectors_writer::{
  KnnVectorsWriter, MergeVectorValues, MergedByteVectorValues, MergedFloat32VectorValues,
  map_old_ord_to_new_ord, merge_byte_vector_values, merge_float_vector_values,
};
use crate::core::codecs::lucene95::off_heap_byte_vector_values;
use crate::core::codecs::lucene95::off_heap_float_vector_values;
use crate::core::codecs::lucene95::ord_to_doc_disi_reader_configuration::OrdToDocDISIReaderConfiguration;
use crate::core::codecs::lucene99::lucene99_flat_vectors_format::DIRECT_MONOTONIC_BLOCK_SHIFT;
use crate::core::codecs::lucene99::lucene99_flat_vectors_format::{
  META_CODEC_NAME, META_EXTENSION, VECTOR_DATA_CODEC_NAME, VECTOR_DATA_EXTENSION, VERSION_CURRENT,
};
use crate::core::codecs::lucene99::lucene99_hnsw_vectors_writer::{
  DefaultRandomVectorScorerSupplier, FieldWriter,
};
use crate::core::index::IndexFileNames;
use crate::core::index::byte_vector_values::ByteVectorValues;
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::docs_with_field_set::DocsWithFieldSet;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::float_vector_values::FloatVectorValues;
use crate::core::index::knn_vector_values::{
  DocIndexIterator, KnnVectorValues, KnnVectorValuesEnm2,
};
use crate::core::index::merge_state::{DocMap as MergeDocMap, MergeState};
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::sorter::DocMap;
use crate::core::index::vector_encoding::VectorEncoding;
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
use crate::core::util::close::Closeable;
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
    D2: Directory,
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
      )
    }));

    match result {
      Ok(Ok(())) => {},
      result => {
        IOUtils::close_resources_while_handling_error((meta.as_mut(), vector_data.as_mut()))?;
        return match result {
          Ok(Err(error)) => Err(error),
          Err(payload) => std::panic::resume_unwind(payload),
          Ok(Ok(())) => Err(LuceneError::illegal_state(
            "flat vectors construction entered failure handling after success",
          )),
        };
      },
    }
    let (meta, vector_data) = match (meta, vector_data) {
      (Some(meta), Some(vector_data)) => (meta, vector_data),
      (mut meta, mut vector_data) => {
        IOUtils::close_resources_while_handling_error((meta.as_mut(), vector_data.as_mut()))?;
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

  fn write_float32_vectors(
    vector_data: &mut O,
    dim: usize,
    vectors: &[VectorValueEnum],
  ) -> Result<()> {
    let byte_size = BitUtil::FLOAT_BYTES;
    let mut buffer = vec![0u8; dim * byte_size];

    for vector in vectors.iter() {
      debug_assert_eq!(vector.len(), dim);
      vector.write_float(&mut buffer)?;
      vector_data.write_bytes_range(&buffer, 0, buffer.len())?;
    }

    Ok(())
  }

  fn write_byte_vectors(vector_data: &mut O, vectors: &[VectorValueEnum]) -> Result<()> {
    for vector in vectors.iter() {
      vector_data.write_bytes_range(vector.as_bytes()?, 0, vector.len())?;
    }

    Ok(())
  }
  fn write_sorted_float32_vectors(
    vector_data: &mut O,
    field_data: &FlatFieldWriter,
    ord_map: &[usize],
    vectors: &[VectorValueEnum],
  ) -> Result<usize>
  where
    O: IndexOutput,
  {
    let vector_data_offset = vector_data.align_file_pointer(BitUtil::FLOAT_BYTES)?;

    let dim = field_data.dim;
    let byte_size = BitUtil::FLOAT_BYTES;
    let mut buffer = vec![0u8; dim * byte_size];

    for &ord in ord_map {
      let vector = &vectors[ord];
      debug_assert_eq!(vector.len(), dim);
      vector.write_float(&mut buffer)?;
      vector_data.write_bytes_range(&buffer, 0, buffer.len())?;
    }

    Ok(vector_data_offset)
  }
  fn write_sorted_byte_vectors(
    vector_data: &mut O,
    ord_map: &[usize],
    vectors: &[VectorValueEnum],
  ) -> Result<usize>
  where
    O: IndexOutput,
  {
    let vector_data_offset = vector_data.align_file_pointer(BitUtil::FLOAT_BYTES)?;
    for &ord in ord_map {
      let vector = &vectors[ord];
      vector_data.write_bytes_range(vector.as_bytes()?, 0, vector.len())?;
    }
    Ok(vector_data_offset)
  }
  fn write_field(
    &mut self,
    field_data_idx: usize,
    max_doc: i32,
    vectors: &[VectorValueEnum],
  ) -> Result<()>
  where
    O: IndexOutput,
  {
    let field_data = self.fields.get_mut(field_data_idx).ok_or_else(|| {
      LuceneError::illegal_argument(format!("Invalid field_data_idx: {}", field_data_idx))
    })?;
    field_data.docs_with_field.finish();
    let vector_data_offset = self.vector_data.align_file_pointer(BitUtil::FLOAT_BYTES)?;

    match field_data.field_info.get_vector_encoding() {
      VectorEncoding::BYTE(_) => {
        Self::write_byte_vectors(&mut self.vector_data, vectors)?;
      },
      VectorEncoding::FLOAT32(_) => {
        Self::write_float32_vectors(&mut self.vector_data, field_data.dim, vectors)?;
      },
    }

    let vector_data_length = self.vector_data.get_file_pointer()? - vector_data_offset;

    write_meta(
      &mut self.meta,
      &mut self.vector_data,
      &field_data.field_info,
      max_doc,
      vector_data_offset as i64,
      vector_data_length as i64,
      field_data.get_docs_with_field_set(),
    )?;

    Ok(())
  }
  fn write_sorting_field<DM>(
    &mut self,
    field_data_idx: usize,
    max_doc: i32,
    sort_map: &DM,
    vectors: &[VectorValueEnum],
  ) -> Result<()>
  where
    DM: DocMap,
  {
    let field_data = self.fields.get_mut(field_data_idx).ok_or_else(|| {
      LuceneError::illegal_argument(format!("Invalid field_data_idx: {}", field_data_idx))
    })?;
    field_data.docs_with_field.finish();

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
    let vector_data_offset = match field_data.field_info.get_vector_encoding() {
      VectorEncoding::BYTE(_) => {
        Self::write_sorted_byte_vectors(&mut self.vector_data, &ord_map, vectors)?
      },
      VectorEncoding::FLOAT32(_) => {
        Self::write_sorted_float32_vectors(&mut self.vector_data, field_data, &ord_map, vectors)?
      },
    };
    let vector_data_length = self.vector_data.get_file_pointer()? - vector_data_offset;

    write_meta(
      &mut self.meta,
      &mut self.vector_data,
      &field_data.field_info,
      max_doc,
      vector_data_offset as i64,
      vector_data_length as i64,
      &new_docs_with_field,
    )?;

    Ok(())
  }
}
impl<O, F> Accountable for Lucene99FlatVectorsWriter<O, F>
where
  F: FlatVectorsScorer,
  O: IndexOutput,
{
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
  F: FlatVectorsScorer,
  O: IndexOutput,
{
  fn close(&mut self) -> Result<()> {
    IOUtils::close([&mut self.meta, &mut self.vector_data], Closeable::close)
  }
}

impl<O, F> KnnVectorsWriter for Lucene99FlatVectorsWriter<O, F>
where
  O: IndexOutput,
  F: FlatVectorsScorer,
{
  fn merge_one_field<D1, D2, CR>(
    &mut self,
    field_info: &Arc<FieldInfo>,
    merge_state: &MergeState<'_, D1, CR>,
    _segment_write_state: &SegmentWriteState<&D2>,
  ) -> Result<()>
  where
    D1: Directory,
    D2: Directory,
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
      vector_data_offset as i64,
      vector_data_length as i64,
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
}

impl<O, F> FlatVectorsWriter for Lucene99FlatVectorsWriter<O, F>
where
  O: IndexOutput,
  F: FlatVectorsScorer + Clone,
{
  type FlatVectorsScorer = F;

  fn get_flat_vector_scorer(&self) -> &Self::FlatVectorsScorer {
    &self.flat_vectors_scorer
  }

  fn flat_add_field(&mut self, field_info: Arc<FieldInfo>) -> Result<usize> {
    let len = self.fields.len();
    let new_field = FlatFieldWriter::new(field_info, len);
    self.fields.push(new_field);
    Ok(len)
  }

  fn flat_flush<DM, F1>(
    &mut self,
    max_doc: i32,
    sort_map: Option<&DM>,
    fields: &[FieldWriter<DefaultRandomVectorScorerSupplier<F1>>],
  ) -> Result<()>
  where
    DM: DocMap,
    F1: FlatVectorsWriter,
  {
    for idx in 0..self.fields.len() {
      let fields = &fields[idx];
      let ss = fields.hnsw_graph_builder.get_scorer_supplier();
      let vectors = ss.get_vector()?;
      if let Some(sm) = sort_map {
        self.write_sorting_field(idx, max_doc, sm, vectors)?;
      } else {
        self.write_field(idx, max_doc, vectors)?;
      }
      self.fields[idx].finish()?;
    }
    Ok(())
  }

  type FlatFieldVectorsWriter = FlatFieldWriter;

  fn get_fields_mut(&mut self) -> &mut [Self::FlatFieldVectorsWriter] {
    self.fields.as_mut()
  }

  type CloseableRandomVectorScorerSupplier<'a, I, D>
    = FlatCloseableRandomVectorScorerSupplier<
    'a,
    <F as FlatVectorsScorer>::RandomVectorScorerSupplier<
      off_heap_byte_vector_values::DenseOffHeapVectorValues<I, F>,
      off_heap_float_vector_values::DenseOffHeapVectorValues<I, F>,
    >,
    D,
    I,
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
    segment_write_state: &SegmentWriteState<&'a D2>,
  ) -> Result<Self::CloseableRandomVectorScorerSupplier<'a, D2::IndexInput, D2>>
  where
    D1: Directory,
    D2: Directory,
    CR: CodecReader,
  {
    let vector_data_offset = self.vector_data.align_file_pointer(BitUtil::FLOAT_BYTES)?;
    let mut temp_vector_data = segment_write_state.directory.create_temp_output(
      self.vector_data.get_name(),
      "temp",
      segment_write_state.context,
    )?;
    let temp_vector_name = temp_vector_data.get_name().to_string();
    let mut vector_data_input = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
      || -> Result<Self::CloseableRandomVectorScorerSupplier<'_, D2::IndexInput, D2>> {
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
        IOUtils::close_one(&mut temp_vector_data)?;

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
          vector_data_offset as i64,
          vector_data_length as i64,
          &docs_with_field,
        )?;

        let vector_values_input = vector_data_input_ref.try_clone()?;
        let random_vector_scorer_supplier = match field_info.get_vector_encoding() {
          VectorEncoding::BYTE(_) => self.flat_vectors_scorer.get_random_vector_scorer_supplier(
            *field_info.get_vector_similarity_function(),
            KnnVectorValuesEnm2::A(off_heap_byte_vector_values::DenseOffHeapVectorValues::new(
              field_info.get_vector_dimension() as usize,
              docs_with_field.cardinality() as usize,
              vector_values_input,
              field_info.get_vector_dimension() as usize * VectorEncoding::BYTE(1).byte_size(),
              self.flat_vectors_scorer.clone(),
              *field_info.get_vector_similarity_function(),
            )),
          )?,
          VectorEncoding::FLOAT32(_) => {
            self.flat_vectors_scorer.get_random_vector_scorer_supplier(
              *field_info.get_vector_similarity_function(),
              KnnVectorValuesEnm2::B(off_heap_float_vector_values::DenseOffHeapVectorValues::new(
                field_info.get_vector_dimension() as usize,
                docs_with_field.cardinality() as usize,
                vector_values_input,
                field_info.get_vector_dimension() as usize * VectorEncoding::FLOAT32(4).byte_size(),
                self.flat_vectors_scorer.clone(),
                *field_info.get_vector_similarity_function(),
              )?),
            )?
          },
        };
        let vector_data_input = vector_data_input
          .take()
          .ok_or_else(|| LuceneError::illegal_state("temporary vector data input is missing"))?;
        Ok(FlatCloseableRandomVectorScorerSupplier::new(
          docs_with_field.cardinality(),
          random_vector_scorer_supplier,
          segment_write_state.directory,
          temp_vector_name.clone(),
          vector_data_input,
        ))
      },
    ));

    match result {
      Ok(Ok(supplier)) => Ok(supplier),
      result => {
        IOUtils::close_resources_while_handling_error((
          &mut temp_vector_data,
          vector_data_input.as_ref(),
        ))?;
        IOUtils::delete_files_ignoring_exceptions(
          segment_write_state.directory,
          std::iter::once(&temp_vector_name),
        );
        match result {
          Ok(Err(error)) => Err(error),
          Err(payload) => std::panic::resume_unwind(payload),
          Ok(Ok(_)) => Err(LuceneError::illegal_state(
            "flat vector merge entered failure handling after success",
          )),
        }
      },
    }
  }
}

fn write_meta<O>(
  meta: &mut O,
  vector_data: &mut O,
  field: &FieldInfo,
  max_doc: i32,
  vector_data_offset: i64,
  vector_data_length: i64,
  docs_with_field: &DocsWithFieldSet,
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
    let value = iter.vector_value(ord)?;
    debug_assert_eq!(value.len(), dim);
    output.write_bytes_range(value.as_bytes()?, 0, value.len())?;
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
  let byte_size = BitUtil::FLOAT_BYTES;
  let mut buffer = vec![0u8; dim * byte_size];

  let mut iter = float_vector_values.iterator()?;
  loop {
    let doc = iter.next_doc()?;
    if doc == NO_MORE_DOCS {
      break;
    }
    let ord: usize = iter.index()?.try_convert()?;
    let value = iter.vector_value(ord)?;
    for (i, &v) in value.as_floats()?.iter().enumerate() {
      let bytes = v.to_le_bytes();
      let start = i * byte_size;
      buffer[start..start + byte_size].copy_from_slice(&bytes);
    }
    output.write_bytes_range(&buffer, 0, buffer.len())?;
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
  idx: usize,
}
impl FlatFieldWriter {
  pub fn new(field_info: Arc<FieldInfo>, idx: usize) -> Self {
    let dim = field_info.get_vector_dimension() as usize;
    Self {
      field_info,
      dim,
      docs_with_field: DocsWithFieldSet::new(),
      finished: false,
      last_doc_id: -1,
      idx,
    }
  }
}

impl Accountable for FlatFieldWriter {
  fn ram_bytes_used(&self) -> Result<i64> {
    self.docs_with_field.ram_bytes_used()
  }
}

impl KnnFieldVectorsWriter for FlatFieldWriter {
  fn copy_value(&self, vector_value: &VectorValueEnum) -> Result<VectorValueEnum> {
    Ok(vector_value.copy_value(0, self.dim))
  }
}
impl FlatFieldVectorsWriter for FlatFieldWriter {
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

  fn flat_add_value<F>(
    &mut self,
    doc_id: i32,
    vector_value: &VectorValueEnum,
    vector: &mut Vec<VectorValueEnum>,
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

    let copy = self.copy_value(vector_value)?;

    self.docs_with_field.add(doc_id)?;
    vector.push(copy);

    self.last_doc_id = doc_id;

    Ok(())
  }
}

pub struct FlatCloseableRandomVectorScorerSupplier<'a, S, D, I>
where
  S: RandomVectorScorerSupplier,
  D: Directory,
  I: IndexInput,
{
  supplier: S,
  num_vectors: i32,
  dir: &'a D,
  temp_file: String,
  vector_data_input: I,
  closed: bool,
}

impl<'a, S, D, I> FlatCloseableRandomVectorScorerSupplier<'a, S, D, I>
where
  S: RandomVectorScorerSupplier,
  D: Directory,
  I: IndexInput,
{
  pub(crate) fn new(
    num_vectors: i32,
    supplier: S,
    dir: &'a D,
    temp_file: String,
    vector_data_input: I,
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

impl<S, D, I> RandomVectorScorerSupplier for FlatCloseableRandomVectorScorerSupplier<'_, S, D, I>
where
  S: RandomVectorScorerSupplier,
  D: Directory,
  I: IndexInput,
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

impl<S, D, I> Closeable for FlatCloseableRandomVectorScorerSupplier<'_, S, D, I>
where
  S: RandomVectorScorerSupplier,
  D: Directory,
  I: IndexInput,
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

impl<S, D, I> CloseableRandomVectorScorerSupplier
  for FlatCloseableRandomVectorScorerSupplier<'_, S, D, I>
where
  S: RandomVectorScorerSupplier,
  D: Directory,
  I: IndexInput,
{
  fn total_vector_count(&self) -> Result<i32> {
    Ok(self.num_vectors)
  }
}
impl<S, D, I> Drop for FlatCloseableRandomVectorScorerSupplier<'_, S, D, I>
where
  S: RandomVectorScorerSupplier,
  D: Directory,
  I: IndexInput,
{
  fn drop(&mut self) {
    let _ = self.close();
  }
}
