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
use crate::core::codecs::knn_field_vectors_writer::VectorValueEnum;
use crate::core::codecs::knn_vectors_reader::KnnVectorsReader;
use crate::core::codecs::lucene95::ord_to_doc_disi_reader_configuration::OrdToDocDISIReaderConfiguration;
use crate::core::codecs::lucene99::lucene99_hnsw_vectors_reader::{
  read_similarity_function, read_vector_encoding,
};
use crate::core::codecs::lucene99::lucene99_scalar_quantized_vector_scorer::ScalarQuantizedVectorsScorer;
use crate::core::codecs::lucene99::lucene99_scalar_quantized_vectors_format::{
  DYNAMIC_CONFIDENCE_INTERVAL, META_CODEC_NAME, META_EXTENSION, VECTOR_DATA_CODEC_NAME,
  VECTOR_DATA_EXTENSION, VERSION_ADD_BITS, VERSION_CURRENT, VERSION_START,
};
use crate::core::codecs::lucene99::off_heap_quantized_byte_vector_values::OffHeapQuantizedByteVectorValuesEnum;
use crate::core::index::IndexFileNames;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::float_vector_values::FloatVectorValues;
use crate::core::index::knn_vector_values::KnnVectorValues;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::index::vector_encoding::VectorEncoding;
use crate::core::index::vector_similarity_function::VectorSimilarityFunction;
use crate::core::search::knn_collector::KnnCollector;
use crate::core::store::check_sum_index_input::ChecksumIndexInput;
use crate::core::store::directory::Directory;
use crate::core::store::{IOContext, IndexInput, ReadAdvice};
use crate::core::util::IOUtils;
use crate::core::util::TryIntoInt;
use crate::core::util::accountable::Accountable;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::bits::Bits;
use crate::core::util::close::CloseableRef;
use crate::core::util::dummy::dummy_hnsw_graph::DummyHnswGraph;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::hnsw::random_vector_scorer::RandomVectorScorerEnum2;
use crate::core::util::quantization::quantized_byte_vector_values::QuantizedByteVectorValues;
use crate::core::util::quantization::quantized_vectors_reader::QuantizedVectorsReader;
use crate::core::util::quantization::scalar_quantizer::ScalarQuantizer;
use std::borrow::Cow;
use std::collections::HashMap;
use std::mem;
use std::sync::Arc;

/// Reads scalar quantized vectors from the index segments.
pub struct Lucene99ScalarQuantizedVectorsReader<I, R, F>
where
  I: IndexInput,
  R: FlatVectorsReader,
  F: FlatVectorsScorer,
{
  fields: HashMap<i32, FieldEntry>,
  quantized_vector_data: Arc<I>,
  raw_vectors_reader: R,
  field_infos: Arc<FieldInfos>,
  flat_vector_scorer: F,
}

impl<I, R, F> Lucene99ScalarQuantizedVectorsReader<I, R, F>
where
  I: IndexInput,
  R: FlatVectorsReader,
  F: ScalarQuantizedVectorsScorer,
{
  pub(crate) fn new<D1, D2>(
    state: &SegmentReadState<D1>,
    raw_vectors_reader: R,
    flat_vector_scorer: F,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self>
  where
    D1: Directory<IndexInput = I>,
    D2: Directory,
  {
    let mut fields = HashMap::<i32, FieldEntry>::new();

    let field_infos = state.field_infos.clone();
    let mut version_meta = -1;
    let meta_file_name =
      IndexFileNames::segment_file_name(&segment_info.name, &state.segment_suffix, META_EXTENSION);
    let mut meta = match state.directory.open_checksum_input(&meta_file_name) {
      Ok(meta) => meta,
      Err(e) => {
        return IOUtils::use_or_suppress_result(Err(e), raw_vectors_reader.close());
      },
    };

    let mut success = false;
    let mut quantized_vector_data = None;

    let result = (|| -> Result<()> {
      let read_result = (|| -> Result<()> {
        version_meta = CodecUtil::check_index_header(
          &mut meta,
          META_CODEC_NAME,
          VERSION_START,
          VERSION_CURRENT,
          segment_info.get_id(),
          &state.segment_suffix,
        )?;
        Self::read_fields(
          &mut meta,
          version_meta,
          state.field_infos.as_ref(),
          &mut fields,
        )
      })();

      match read_result {
        Ok(()) => CodecUtil::check_footer(&mut meta).map(|_| ()),
        Err(e) => Err(CodecUtil::check_footer_with_error(&mut meta, e)),
      }?;

      quantized_vector_data = Some(Self::open_data_input(
        state,
        version_meta,
        VECTOR_DATA_EXTENSION,
        VECTOR_DATA_CODEC_NAME,
        // Quantized vectors are accessed randomly from their node ID stored in the HNSW
        // graph.
        &state.context.with_read_advice_self(ReadAdvice::Random)?,
        segment_info,
      )?);
      success = true;
      Ok(())
    })();
    let result = IOUtils::use_or_suppress_result(result, meta.close());

    if let Err(e) = result {
      let mut result: Result<()> = Err(e);
      if !success {
        if let Some(input) = quantized_vector_data {
          result = IOUtils::use_or_suppress_result(result, input.close());
        }
        result = IOUtils::use_or_suppress_result(result, raw_vectors_reader.close());
      }
      return match result {
        Ok(()) => Err(LuceneError::illegal_state(
          "constructor failed but close handling cleared the error",
        )),
        Err(e) => Err(e),
      };
    }

    Ok(Self {
      fields,
      quantized_vector_data: Arc::new(
        quantized_vector_data
          .ok_or_else(|| LuceneError::illegal_state("quantizedVectorData was not initialized"))?,
      ),
      raw_vectors_reader,
      field_infos,
      flat_vector_scorer,
    })
  }

  fn read_fields<M>(
    meta: &mut M,
    version_meta: i32,
    infos: &FieldInfos,
    fields: &mut HashMap<i32, FieldEntry>,
  ) -> Result<()>
  where
    M: ChecksumIndexInput,
  {
    let mut field_number = meta.read_int()?;
    while field_number != -1 {
      let info = infos.field_info_by_number(field_number)?.ok_or_else(|| {
        LuceneError::corrupt_index(format!("Invalid field number: {}", field_number))
      })?;
      let field_entry = Self::read_field(meta, version_meta, info.as_ref())?;
      Self::validate_field_entry(info.as_ref(), &field_entry)?;
      fields.insert(info.number, field_entry);
      field_number = meta.read_int()?;
    }
    Ok(())
  }

  fn validate_field_entry(info: &FieldInfo, field_entry: &FieldEntry) -> Result<()> {
    let dimension = info.get_vector_dimension();
    if dimension as usize != field_entry.dimension {
      return Err(LuceneError::illegal_state(format!(
        "Inconsistent vector dimension for field=\"{}\"; {} != {}",
        info.name, dimension, field_entry.dimension
      )));
    }

    let quantized_vector_bytes = if field_entry.bits <= 4 && field_entry.compress {
      // two dimensions -> one byte
      ((dimension as usize + 1) >> 1) + BitUtil::FLOAT_BYTES
    } else {
      // one dimension -> one byte
      dimension as usize + BitUtil::FLOAT_BYTES
    };
    let num_quantized_vector_bytes = quantized_vector_bytes
      .checked_mul(field_entry.size)
      .ok_or_else(|| LuceneError::illegal_state("numQuantizedVectorBytes overflow"))?;
    if num_quantized_vector_bytes != field_entry.vector_data_length {
      return Err(LuceneError::illegal_state(format!(
        "Quantized vector data length {} not matching size={} * (dim={} + 4) = {}",
        field_entry.vector_data_length, field_entry.size, dimension, num_quantized_vector_bytes
      )));
    }

    Ok(())
  }

  fn get_field_entry(&self, field: &str) -> Result<&FieldEntry> {
    let info = self
      .field_infos
      .field_info_by_name(field)?
      .ok_or_else(|| LuceneError::illegal_argument(format!("field=\"{}\" not found", field)))?;
    let field_entry = self
      .fields
      .get(&info.number)
      .ok_or_else(|| LuceneError::illegal_argument(format!("field=\"{}\" not found", field)))?;
    if field_entry.vector_encoding != VectorEncoding::FLOAT32(4) {
      return Err(LuceneError::illegal_argument(format!(
        "field=\"{}\" is encoded as: {:?} expected: {:?}",
        field,
        field_entry.vector_encoding,
        VectorEncoding::FLOAT32(4)
      )));
    }
    Ok(field_entry)
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
    let result = (|| {
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
    })();
    match result {
      Ok(()) => Ok(input),
      Err(e) => IOUtils::use_or_suppress_result(Err(e), input.close()),
    }
  }

  fn read_field<T>(input: &mut T, version_meta: i32, info: &FieldInfo) -> Result<FieldEntry>
  where
    T: IndexInput,
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
      version_meta,
      vector_encoding,
      *info.get_vector_similarity_function(),
    )
  }
}

impl<I, R, F> CloseableRef for Lucene99ScalarQuantizedVectorsReader<I, R, F>
where
  I: IndexInput,
  R: FlatVectorsReader,
  F: FlatVectorsScorer,
{
  fn close(&self) -> Result<()> {
    IOUtils::close_refs_tuple((
      Some(&self.quantized_vector_data),
      Some(&self.raw_vectors_reader),
    ))
  }
}

impl<I, R, F> HnswGraphProvider for Lucene99ScalarQuantizedVectorsReader<I, R, F>
where
  I: IndexInput,
  R: FlatVectorsReader,
  F: FlatVectorsScorer,
{
  type HnswGraph = DummyHnswGraph;
}

impl<I, R, F> KnnVectorsReader for Lucene99ScalarQuantizedVectorsReader<I, R, F>
where
  I: IndexInput,
  R: FlatVectorsReader,
  F: ScalarQuantizedVectorsScorer,
  OffHeapQuantizedByteVectorValuesEnum<Arc<I>, F>: QuantizedByteVectorValues,
{
  fn check_integrity(&self) -> Result<()> {
    self.raw_vectors_reader.check_integrity()?;
    CodecUtil::checksum_entire_file(self.quantized_vector_data.as_ref())?;
    Ok(())
  }

  type FloatVectorValues =
    QuantizedVectorValues<R::FloatVectorValues, OffHeapQuantizedByteVectorValuesEnum<Arc<I>, F>>;

  fn get_float_vector_values(&self, field: &str) -> Result<Self::FloatVectorValues> {
    let field_entry = self.get_field_entry(field)?;
    let raw_vector_values = self.raw_vectors_reader.get_float_vector_values(field)?;
    let quantized_vector_values = OffHeapQuantizedByteVectorValuesEnum::load(
      field_entry.ord_to_doc.clone(),
      field_entry.dimension,
      field_entry.size,
      field_entry.scalar_quantizer.clone(),
      field_entry.similarity_function,
      self.flat_vector_scorer.clone(),
      field_entry.compress,
      field_entry.vector_data_offset,
      field_entry.vector_data_length,
      self.quantized_vector_data.clone(),
    )?;
    Ok(QuantizedVectorValues::new(
      raw_vector_values,
      quantized_vector_values,
    ))
  }

  type ByteVectorValues = R::ByteVectorValues;

  fn get_byte_vector_values(&self, field: &str) -> Result<Self::ByteVectorValues> {
    self.raw_vectors_reader.get_byte_vector_values(field)
  }

  fn get_quantization_state(&self, field: &str) -> Result<Option<ScalarQuantizer>> {
    QuantizedVectorsReader::get_quantization_state(self, field)
  }

  fn is_flat_vectors_reader(&self, _field: &str) -> bool {
    true
  }

  fn search_f32<B, K>(
    &self,
    _field: &str,
    _target: Vec<f32>,
    _knn_collector: &mut K,
    _accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
  {
    // don't scan stored field data. If we didn't index it, produce no search results
    Ok(())
  }

  fn search_u8<B, K>(
    &self,
    _field: &str,
    _target: Vec<u8>,
    _knn_collector: &mut K,
    _accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
  {
    // don't scan stored field data. If we didn't index it, produce no search results
    Ok(())
  }
}

impl<I, R, F> Accountable for Lucene99ScalarQuantizedVectorsReader<I, R, F>
where
  I: IndexInput,
  R: FlatVectorsReader,
  F: FlatVectorsScorer,
{
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(
      self
        .fields
        .ram_bytes_used()?
        .saturating_add(self.raw_vectors_reader.ram_bytes_used()?),
    )
  }
}

impl<I, R, F> FlatVectorsReader for Lucene99ScalarQuantizedVectorsReader<I, R, F>
where
  I: IndexInput,
  R: FlatVectorsReader,
  F: ScalarQuantizedVectorsScorer,
  OffHeapQuantizedByteVectorValuesEnum<Arc<I>, F>: QuantizedByteVectorValues,
{
  type FlatVectorsScorer = F;

  fn get_flat_vector_scorer(&self) -> &Self::FlatVectorsScorer {
    &self.flat_vector_scorer
  }

  type RandomVectorScorerF32 = RandomVectorScorerEnum2<
    R::RandomVectorScorerF32,
    F::QuantizedRandomVectorScorer<OffHeapQuantizedByteVectorValuesEnum<Arc<I>, F>>,
  >;

  fn get_random_vector_scorer_f32(
    &self,
    field: &str,
    target: Vec<f32>,
  ) -> Result<Self::RandomVectorScorerF32> {
    let field_entry = self.get_field_entry(field)?;
    if field_entry.scalar_quantizer.is_none() {
      return self
        .raw_vectors_reader
        .get_random_vector_scorer_f32(field, target)
        .map(RandomVectorScorerEnum2::A);
    }
    let vector_values = OffHeapQuantizedByteVectorValuesEnum::load(
      field_entry.ord_to_doc.clone(),
      field_entry.dimension,
      field_entry.size,
      field_entry.scalar_quantizer.clone(),
      field_entry.similarity_function,
      self.flat_vector_scorer.clone(),
      field_entry.compress,
      field_entry.vector_data_offset,
      field_entry.vector_data_length,
      self.quantized_vector_data.clone(),
    )?;
    self
      .flat_vector_scorer
      .get_random_vector_scorer_f32_quantized(
        field_entry.similarity_function,
        vector_values,
        target,
      )
      .map(RandomVectorScorerEnum2::B)
  }

  type RandomVectorScorerU8 = R::RandomVectorScorerU8;

  fn get_random_vector_scorer_u8(
    &self,
    field: &str,
    target: Vec<u8>,
  ) -> Result<Self::RandomVectorScorerU8> {
    self
      .raw_vectors_reader
      .get_random_vector_scorer_u8(field, target)
  }
}

impl<I, R, F> QuantizedVectorsReader for Lucene99ScalarQuantizedVectorsReader<I, R, F>
where
  I: IndexInput,
  R: FlatVectorsReader,
  F: ScalarQuantizedVectorsScorer,
  OffHeapQuantizedByteVectorValuesEnum<Arc<I>, F>: QuantizedByteVectorValues,
{
  type QuantizedByteVectorValues = OffHeapQuantizedByteVectorValuesEnum<Arc<I>, F>;

  fn get_quantized_vector_values(&self, field: &str) -> Result<Self::QuantizedByteVectorValues> {
    let field_entry = self.get_field_entry(field)?;
    OffHeapQuantizedByteVectorValuesEnum::load(
      field_entry.ord_to_doc.clone(),
      field_entry.dimension,
      field_entry.size,
      field_entry.scalar_quantizer.clone(),
      field_entry.similarity_function,
      self.flat_vector_scorer.clone(),
      field_entry.compress,
      field_entry.vector_data_offset,
      field_entry.vector_data_length,
      self.quantized_vector_data.clone(),
    )
  }

  fn get_quantization_state(&self, field: &str) -> Result<Option<ScalarQuantizer>> {
    let field_entry = self.get_field_entry(field)?;
    Ok(field_entry.scalar_quantizer.clone())
  }
}

struct FieldEntry {
  similarity_function: VectorSimilarityFunction,
  vector_encoding: VectorEncoding,
  dimension: usize,
  vector_data_offset: usize,
  vector_data_length: usize,
  scalar_quantizer: Option<ScalarQuantizer>,
  size: usize,
  bits: u8,
  compress: bool,
  ord_to_doc: Arc<OrdToDocDISIReaderConfiguration>,
}

impl FieldEntry {
  fn create<I>(
    input: &mut I,
    version_meta: i32,
    vector_encoding: VectorEncoding,
    similarity_function: VectorSimilarityFunction,
  ) -> Result<Self>
  where
    I: IndexInput,
  {
    let vector_data_offset = input.read_vlong()?.try_convert()?;
    let vector_data_length = input.read_vlong()?.try_convert()?;
    let dimension = input.read_vint()?.try_convert()?;
    let size = input.read_int()?.try_convert()?;
    let scalar_quantizer;
    let bits;
    let compress;
    if size > 0 {
      if version_meta < VERSION_ADD_BITS {
        let float_bits = input.read_int()?; // confidenceInterval, unused
        if float_bits == -1 {
          // indicates a null confidence interval
          return Err(LuceneError::corrupt_index(
            "Missing confidence interval for scalar quantizer",
          ));
        }
        let confidence_interval = f32::from_bits(float_bits as u32);
        // indicates a dynamic interval, which shouldn't be provided in this version
        if confidence_interval == DYNAMIC_CONFIDENCE_INTERVAL {
          return Err(LuceneError::corrupt_index(format!(
            "Invalid confidence interval for scalar quantizer: {}",
            confidence_interval
          )));
        }
        bits = 7;
        compress = false;
        let min_quantile = f32::from_bits(input.read_int()? as u32);
        let max_quantile = f32::from_bits(input.read_int()? as u32);
        scalar_quantizer = Some(ScalarQuantizer::new(min_quantile, max_quantile, 7)?);
      } else {
        input.read_int()?; // confidenceInterval, unused
        bits = input.read_byte()?;
        compress = input.read_byte()? == 1;
        let min_quantile = f32::from_bits(input.read_int()? as u32);
        let max_quantile = f32::from_bits(input.read_int()? as u32);
        scalar_quantizer = Some(ScalarQuantizer::new(min_quantile, max_quantile, bits)?);
      }
    } else {
      scalar_quantizer = None;
      bits = 7;
      compress = false;
    }
    let ord_to_doc = OrdToDocDISIReaderConfiguration::from_stored_meta(input, size as i32)?;
    Ok(Self {
      similarity_function,
      vector_encoding,
      dimension,
      vector_data_offset,
      vector_data_length,
      scalar_quantizer,
      size,
      bits,
      compress,
      ord_to_doc: Arc::new(ord_to_doc),
    })
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

pub struct QuantizedVectorValues<R, Q>
where
  R: FloatVectorValues,
  Q: QuantizedByteVectorValues,
{
  raw_vector_values: R,
  quantized_vector_values: Q,
}

impl<R, Q> QuantizedVectorValues<R, Q>
where
  R: FloatVectorValues,
  Q: QuantizedByteVectorValues,
{
  fn new(raw_vector_values: R, quantized_vector_values: Q) -> Self {
    Self {
      raw_vector_values,
      quantized_vector_values,
    }
  }
}

impl<R, Q> KnnVectorValues for QuantizedVectorValues<R, Q>
where
  R: FloatVectorValues,
  Q: QuantizedByteVectorValues,
{
  fn dimension(&self) -> usize {
    self.raw_vector_values.dimension()
  }

  fn size(&self) -> usize {
    self.raw_vector_values.size()
  }

  fn ord_to_doc(&self, ord: usize) -> Result<usize> {
    self.raw_vector_values.ord_to_doc(ord)
  }

  type KnnVectorValues = QuantizedVectorValues<R::FloatVectorValues, Q::QuantizedByteVectorValues>;

  fn copy(&self) -> Result<Self::KnnVectorValues> {
    let raw_vector_values = self.raw_vector_values.float_copy()?.ok_or_else(|| {
      LuceneError::unsupported_operation("raw vector values do not support float_copy")
    })?;
    let quantized_vector_values = QuantizedByteVectorValues::copy(&self.quantized_vector_values)?;
    Ok(QuantizedVectorValues::new(
      raw_vector_values,
      quantized_vector_values,
    ))
  }

  fn get_encoding(&self) -> VectorEncoding {
    KnnVectorValues::get_encoding(&self.raw_vector_values)
  }

  type Bits<'a, B>
    = R::Bits<'a, B>
  where
    B: Bits,
    Self: 'a;

  fn get_accept_ords<'a, B>(&'a self, accept_docs: Option<B>) -> Option<Self::Bits<'a, B>>
  where
    B: Bits,
  {
    self.raw_vector_values.get_accept_ords(accept_docs)
  }

  type DocIndexIterator = R::DocIndexIterator;

  fn iterator(&self) -> Result<Self::DocIndexIterator> {
    self.raw_vector_values.iterator()
  }
}

impl<R, Q> FloatVectorValues for QuantizedVectorValues<R, Q>
where
  R: FloatVectorValues,
  Q: QuantizedByteVectorValues,
{
  fn vector_value(&self, ord: usize) -> Result<Cow<'_, VectorValueEnum>> {
    self.raw_vector_values.vector_value(ord)
  }

  type FloatVectorValues =
    QuantizedVectorValues<R::FloatVectorValues, Q::QuantizedByteVectorValues>;

  fn float_copy(&self) -> Result<Option<Self::FloatVectorValues>> {
    Ok(match self.raw_vector_values.float_copy()? {
      Some(raw_vector_values) => Some(QuantizedVectorValues::new(
        raw_vector_values,
        QuantizedByteVectorValues::copy(&self.quantized_vector_values)?,
      )),
      None => None,
    })
  }

  type VectorScorer = Q::QuantizedVectorScorer;

  fn scorer(&self, query: Vec<f32>) -> Result<Option<Self::VectorScorer>> {
    QuantizedByteVectorValues::scorer(&self.quantized_vector_values, &query)
  }

  fn get_encoding(&self) -> VectorEncoding {
    FloatVectorValues::get_encoding(&self.raw_vector_values)
  }
}
