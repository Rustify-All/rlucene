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
use crate::core::codecs::hnsw::flat_vectors_scorer::FlatVectorsScorer;
use crate::core::codecs::hnsw::hnsw_graph_provider::HnswGraphProvider;
use crate::core::codecs::knn_vectors_reader::KnnVectorsReader;
use crate::core::codecs::lucene95::off_heap_byte_vector_values::OffHeapByteVectorValuesEnum;
use crate::core::codecs::lucene95::off_heap_float_vector_values::OffHeapFloatVectorValuesEnum;
use crate::core::codecs::lucene95::ord_to_doc_disi_reader_configuration::OrdToDocDISIReaderConfiguration;
use crate::core::codecs::lucene99::lucene99_flat_vectors_format::{
  META_CODEC_NAME, META_EXTENSION, VECTOR_DATA_CODEC_NAME, VECTOR_DATA_EXTENSION, VERSION_CURRENT,
  VERSION_START,
};
use crate::core::codecs::lucene99::lucene99_hnsw_vectors_reader::{
  read_similarity_function, read_vector_encoding,
};
use crate::core::index::IndexFileNames;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::index::vector_encoding::VectorEncoding;
use crate::core::index::vector_similarity_function::VectorSimilarityFunction;
use crate::core::search::knn_collector::KnnCollector;
use crate::core::store::check_sum_index_input::ChecksumIndexInput;
use crate::core::store::directory::Directory;
use crate::core::store::{IOContext, IndexInput, ReadAdvice};
use crate::core::util::IOUtils;
use crate::core::util::accountable::Accountable;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::bits::Bits;
use crate::core::util::close::CloseableRef;
use crate::core::util::dummy::dummy_hnsw_graph::DummyHnswGraph;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::collections::HashMap;
use std::mem;
use std::sync::Arc;

/// Reads vectors from the index segments.
pub struct Lucene99FlatVectorsReader<I, F>
where
  I: IndexInput,
  F: FlatVectorsScorer,
{
  field_infos: Arc<FieldInfos>,
  fields: HashMap<i32, FieldEntry>,
  vector_data: Arc<I>,
  vector_scorer: F,
}
impl<I, F> Lucene99FlatVectorsReader<I, F>
where
  I: IndexInput,
  F: FlatVectorsScorer + Clone,
{
  pub fn new<D1, D2>(
    state: &SegmentReadState<D1>,
    scorer: F,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self>
  where
    D1: Directory<IndexInput = I>,
    D2: Directory,
  {
    let mut fields = HashMap::<i32, FieldEntry>::new();
    let version_meta = Self::read_metadata(state, segment_info, &mut fields)?;

    let context = &state.context.with_read_advice_self(ReadAdvice::Random)?;
    let vector_index = Self::open_data_input(
      state,
      version_meta,
      VECTOR_DATA_EXTENSION,
      VECTOR_DATA_CODEC_NAME,
      context,
      segment_info,
    )?;
    Ok(Self {
      field_infos: state.field_infos.clone(),
      fields,
      vector_data: Arc::new(vector_index),
      vector_scorer: scorer,
    })
  }
  fn read_metadata<D1, D2>(
    state: &SegmentReadState<D1>,
    segment_info: &SegmentInfo<D2>,
    fields: &mut HashMap<i32, FieldEntry>,
  ) -> Result<i32>
  where
    D1: Directory,
    D2: Directory,
  {
    let meta_file_name =
      IndexFileNames::segment_file_name(&segment_info.name, &state.segment_suffix, META_EXTENSION);

    let mut meta = state.directory.open_checksum_input(&meta_file_name)?;

    let mut footer_attempted = false;
    let mut result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<i32> {
      let result = (|| {
        let version_meta = CodecUtil::check_index_header(
          &mut meta,
          META_CODEC_NAME,
          VERSION_START,
          VERSION_CURRENT,
          segment_info.get_id(),
          &state.segment_suffix,
        )?;

        Self::read_fields(&mut meta, &state.field_infos, fields)?;
        Ok(version_meta)
      })();
      footer_attempted = true;
      match result {
        Ok(version_meta) => {
          CodecUtil::check_footer(&mut meta)?;
          Ok(version_meta)
        },
        Err(error) => Err(CodecUtil::check_footer_with_error(&mut meta, error)),
      }
    }));

    let footer_error = if let Err(payload) = &result
      && !footer_attempted
    {
      let error = LuceneError::tragedy_from_panic(
        "panic while reading flat vector metadata",
        payload.as_ref(),
      );
      Some(CodecUtil::check_footer_with_error(&mut meta, error))
    } else {
      None
    };
    if let Some(error @ LuceneError::CorruptIndex(_)) = footer_error {
      result = Ok(Err(error));
    }

    let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| meta.close()));
    IOUtils::use_or_suppress_caught_result(result, close_result)
  }
  fn open_data_input<D1, D2>(
    state: &SegmentReadState<D1>,
    version_meta: i32,
    file_extension: &str,
    codec_name: &str,
    context: &IOContext,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<I>
  where
    D1: Directory<IndexInput = I>,
    D2: Directory,
  {
    let file_name =
      IndexFileNames::segment_file_name(&segment_info.name, &state.segment_suffix, file_extension);
    let mut input = state.directory.open_input(&file_name, context)?;
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
      Ok(())
    }));
    match result {
      Ok(Ok(())) => Ok(input),
      result => {
        IOUtils::close_resources_while_handling_error(&input)?;
        match result {
          Ok(Err(error)) => Err(error),
          Err(payload) => std::panic::resume_unwind(payload),
          Ok(Ok(())) => Err(LuceneError::illegal_state(
            "flat vector data validation entered failure handling after success",
          )),
        }
      },
    }
  }
  fn read_fields(
    meta: &mut impl ChecksumIndexInput,
    infos: &FieldInfos,
    fields: &mut HashMap<i32, FieldEntry>,
  ) -> Result<()> {
    let mut field_number = meta.read_int()?;
    while field_number != -1 {
      let info = match infos.field_info_by_number(field_number)? {
        Some(info) => info,
        None => {
          return Err(LuceneError::corrupt_index(format!(
            "Invalid field number: {}",
            field_number
          )));
        },
      };

      let field_entry = FieldEntry::create(meta, &info)?;
      fields.insert(info.number, field_entry);

      field_number = meta.read_int()?;
    }
    Ok(())
  }
  fn get_field_entry(&self, field: &str, expected_encoding: VectorEncoding) -> Result<&FieldEntry> {
    let info = match self.field_infos.field_info_by_name(field)? {
      Some(info) => info,
      None => {
        return Err(LuceneError::illegal_argument(format!(
          "field=\"{}\" not found",
          field
        )));
      },
    };

    let field_entry = match self.fields.get(&info.number) {
      Some(entry) => entry,
      None => {
        return Err(LuceneError::illegal_argument(format!(
          "field=\"{}\" not found",
          field
        )));
      },
    };

    if field_entry.vector_encoding != expected_encoding {
      return Err(LuceneError::illegal_argument(format!(
        "field=\"{}\" is encoded as: {:?} expected: {:?}",
        field, field_entry.vector_encoding, expected_encoding
      )));
    }

    Ok(field_entry)
  }
}

impl<I, F> CloseableRef for Lucene99FlatVectorsReader<I, F>
where
  I: IndexInput,
  F: FlatVectorsScorer,
{
  fn close(&self) -> Result<()> {
    CloseableRef::close(&self.vector_data)
  }
}

impl<I, F> HnswGraphProvider for Lucene99FlatVectorsReader<I, F>
where
  I: IndexInput,
  F: FlatVectorsScorer,
{
  type HnswGraph = DummyHnswGraph;
}

impl<I, F> KnnVectorsReader for Lucene99FlatVectorsReader<I, F>
where
  F: FlatVectorsScorer + Clone,
  I: IndexInput,
{
  fn check_integrity(&self) -> Result<()> {
    CodecUtil::checksum_entire_file(self.vector_data.as_ref())?;
    Ok(())
  }

  type FloatVectorValues = OffHeapFloatVectorValuesEnum<Arc<I>, F>;

  fn get_float_vector_values(&self, field: &str) -> Result<Self::FloatVectorValues> {
    let field_entry = self.get_field_entry(field, VectorEncoding::FLOAT32(4))?;
    OffHeapFloatVectorValuesEnum::load(
      field_entry.similarity_function,
      self.vector_scorer.clone(),
      field_entry.ord_to_doc.clone(),
      field_entry.vector_encoding,
      field_entry.dimension,
      field_entry.vector_data_offset,
      field_entry.vector_data_length,
      self.vector_data.clone(),
    )
  }

  type ByteVectorValues = OffHeapByteVectorValuesEnum<Arc<I>, F>;

  fn get_byte_vector_values(&self, field: &str) -> Result<Self::ByteVectorValues> {
    let field_entry = self.get_field_entry(field, VectorEncoding::BYTE(1))?;
    OffHeapByteVectorValuesEnum::load(
      field_entry.similarity_function,
      self.vector_scorer.clone(),
      field_entry.ord_to_doc.clone(),
      field_entry.vector_encoding,
      field_entry.dimension,
      field_entry.vector_data_offset,
      field_entry.vector_data_length,
      self.vector_data.clone(),
    )
  }

  fn is_flat_vectors_reader(&self) -> bool {
    true
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
    FlatVectorsReader::search_f32(self, field, target, knn_collector, accept_docs)
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
    FlatVectorsReader::search_u8(self, field, target, knn_collector, accept_docs)
  }

  fn get_merge_instance(&self) -> Result<Option<Self>>
  where
    Self: Sized,
  {
    self
      .vector_data
      .update_read_advice(ReadAdvice::Sequential)?;
    Ok(None)
  }

  fn finish_merge(&self) -> Result<()> {
    self
      .vector_data
      .update_read_advice(ReadAdvice::Sequential)?;
    Ok(())
  }
}

impl<I, F> Accountable for Lucene99FlatVectorsReader<I, F>
where
  F: FlatVectorsScorer + Clone,
  I: IndexInput,
{
  fn ram_bytes_used(&self) -> Result<i64> {
    self.fields.ram_bytes_used()
  }
}

impl<I, F> FlatVectorsReader for Lucene99FlatVectorsReader<I, F>
where
  I: IndexInput,
  F: FlatVectorsScorer + Clone,
{
  type FlatVectorsScorer = F;

  fn get_flat_vector_scorer(&self) -> &Self::FlatVectorsScorer {
    &self.vector_scorer
  }

  type RandomVectorScorerF32 = F::RandomVectorScorerF32<OffHeapFloatVectorValuesEnum<Arc<I>, F>>;

  fn get_random_vector_scorer_f32(
    &self,
    field: &str,
    target: Vec<f32>,
  ) -> Result<Self::RandomVectorScorerF32> {
    let field_entry = self.get_field_entry(field, VectorEncoding::FLOAT32(4))?;
    let off_heap_float_vector_values = OffHeapFloatVectorValuesEnum::load(
      field_entry.similarity_function,
      self.vector_scorer.clone(),
      field_entry.ord_to_doc.clone(),
      field_entry.vector_encoding,
      field_entry.dimension,
      field_entry.vector_data_offset,
      field_entry.vector_data_length,
      self.vector_data.clone(),
    )?;
    self.vector_scorer.get_random_vector_scorer_f32(
      field_entry.similarity_function,
      off_heap_float_vector_values,
      target,
    )
  }

  type RandomVectorScorerU8 = F::RandomVectorScorerU8<OffHeapByteVectorValuesEnum<Arc<I>, F>>;

  fn get_random_vector_scorer_u8(
    &self,
    field: &str,
    target: Vec<u8>,
  ) -> Result<Self::RandomVectorScorerU8> {
    let field_entry = self.get_field_entry(field, VectorEncoding::BYTE(1))?;
    let off_heap_float_vector_values = OffHeapByteVectorValuesEnum::load(
      field_entry.similarity_function,
      self.vector_scorer.clone(),
      field_entry.ord_to_doc.clone(),
      field_entry.vector_encoding,
      field_entry.dimension,
      field_entry.vector_data_offset,
      field_entry.vector_data_length,
      self.vector_data.clone(),
    )?;
    self.vector_scorer.get_random_vector_scorer_u8(
      field_entry.similarity_function,
      off_heap_float_vector_values,
      target,
    )
  }
}

struct FieldEntry {
  similarity_function: VectorSimilarityFunction,
  vector_encoding: VectorEncoding,
  vector_data_offset: usize,
  vector_data_length: usize,
  size: usize,
  dimension: usize,
  ord_to_doc: Arc<OrdToDocDISIReaderConfiguration>,
}
impl FieldEntry {
  pub fn create(input: &mut impl IndexInput, info: &FieldInfo) -> Result<Self> {
    let vector_encoding = read_vector_encoding(input)?;
    let similarity_function = read_similarity_function(input)?;
    let vector_data_offset = input.read_vlong()? as usize;
    let vector_data_length = input.read_vlong()? as usize;
    let dimension = input.read_vint()? as usize;
    let size = input.read_int()? as usize;
    let ord_to_doc = OrdToDocDISIReaderConfiguration::from_stored_meta(input, size as i32)?;

    let entry = Self {
      similarity_function,
      vector_encoding,
      vector_data_offset,
      vector_data_length,
      size,
      dimension,
      ord_to_doc: Arc::new(ord_to_doc),
    };

    entry.validate(info)?;
    Ok(entry)
  }
  pub fn validate(&self, info: &FieldInfo) -> Result<()> {
    if self.similarity_function != *info.get_vector_similarity_function() {
      return Err(LuceneError::illegal_state(format!(
        "Inconsistent vector similarity function for field=\"{}\"; {:?} != {:?}",
        info.name,
        self.similarity_function,
        info.get_vector_similarity_function()
      )));
    }

    let info_vector_dimension = info.get_vector_dimension();
    if info_vector_dimension as usize != self.dimension {
      return Err(LuceneError::illegal_state(format!(
        "Inconsistent vector dimension for field=\"{}\"; {} != {}",
        info.name, info_vector_dimension, self.dimension
      )));
    }

    let byte_size = match info.get_vector_encoding() {
      VectorEncoding::BYTE(_) => BitUtil::BYTE_BYTES,
      VectorEncoding::FLOAT32(_) => BitUtil::FLOAT_BYTES,
    };

    let vector_bytes = (info_vector_dimension as i64)
      .checked_mul(byte_size as i64)
      .ok_or_else(|| LuceneError::illegal_state("vector_bytes overflow"))?;

    let num_bytes = vector_bytes
      .checked_mul(self.size as i64)
      .ok_or_else(|| LuceneError::illegal_state("num_bytes overflow"))?;

    if num_bytes != self.vector_data_length as i64 {
      return Err(LuceneError::illegal_state(format!(
        "Vector data length {} not matching size={} * dim={} * byteSize={} = {}",
        self.vector_data_length, self.size, self.dimension, byte_size, num_bytes
      )));
    }

    Ok(())
  }
}

impl Accountable for FieldEntry {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(
      (mem::size_of_val(self.ord_to_doc.as_ref()) as i64)
        .saturating_add(self.ord_to_doc.ram_bytes_used()?),
    )
  }
}
