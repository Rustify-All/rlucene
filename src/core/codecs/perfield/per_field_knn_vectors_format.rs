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

use crate::core::codecs::hnsw::hnsw_graph_provider::HnswGraphProvider;
use crate::core::codecs::knn_field_vectors_writer::VectorValueEnum;
use crate::core::codecs::knn_vectors_format::KnnVectorsFormat;
use crate::core::codecs::knn_vectors_reader::KnnVectorsReader;
use crate::core::codecs::knn_vectors_writer::KnnVectorsWriter;
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::index_reader::Identity;
use crate::core::index::merge_state::MergeState;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::sorter::DocMap;
use crate::core::search::knn_collector::KnnCollector;
use crate::core::store::directory::Directory;
use crate::core::store::{IndexInput, IndexOutput};
use crate::core::util::accountable::Accountable;
use crate::core::util::bits::Bits;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::quantization::scalar_quantizer::ScalarQuantizer;
use crate::core::util::{HasIdentity, IOUtils};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

/// Name of this [`KnnVectorsFormat`].
pub const PER_FIELD_NAME: &str = "PerFieldVectors90";

/// [`FieldInfo`] attribute name used to store the format name for each field.
pub const PER_FIELD_FORMAT_KEY: &str = "PerFieldKnnVectorsFormat.format";

/// [`FieldInfo`] attribute name used to store the segment suffix name for each field.
pub const PER_FIELD_SUFFIX_KEY: &str = "PerFieldKnnVectorsFormat.suffix";

/// Static-dispatch access to the format selection needed by
/// [`PerFieldKnnVectorsFormat`].
pub trait PerFieldKnnVectorsFormatBase {
  type Format: KnnVectorsFormat;

  /// Returns the numeric vector format that should be used for writing new
  /// segments of `field`.
  ///
  /// The field-to-format mapping is written to the index, so this method is
  /// only invoked when writing, not when reading.
  fn get_knn_vectors_format_for_field(&self, field: &str) -> Result<&Self::Format>;
}

/// Enables per-field numeric vector support.
///
/// The selected numeric vector format's name is written into the index. In
/// order for a field to be read, that name must resolve to the same
/// implementation through [`KnnVectorsFormat::for_name`].
///
/// Files written by each numeric vector format have an additional suffix
/// containing the format name. For example, in a per-field configuration, a
/// file named `_1.dat` would instead look like `_1_Lucene40_0.dat`.
///
/// # Experimental
pub struct PerFieldKnnVectorsFormat<B>
where
  B: PerFieldKnnVectorsFormatBase,
{
  base: Arc<B>,
  identity: Identity,
}

impl<B> Clone for PerFieldKnnVectorsFormat<B>
where
  B: PerFieldKnnVectorsFormatBase,
{
  fn clone(&self) -> Self {
    Self {
      base: Arc::clone(&self.base),
      identity: self.identity.clone(),
    }
  }
}

impl<B> PerFieldKnnVectorsFormat<B>
where
  B: PerFieldKnnVectorsFormatBase,
{
  /// Sole constructor.
  pub fn new(base: B) -> Self {
    Self {
      base: Arc::new(base),
      identity: Identity::new(),
    }
  }
}

impl<B> HasIdentity for PerFieldKnnVectorsFormat<B>
where
  B: PerFieldKnnVectorsFormatBase,
{
  fn identity(&self) -> &Identity {
    &self.identity
  }
}

impl<B> Display for PerFieldKnnVectorsFormat<B>
where
  B: PerFieldKnnVectorsFormatBase,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "KnnVectorsFormat(name={PER_FIELD_NAME})")
  }
}

struct WriterAndSuffix<KVW>
where
  KVW: KnnVectorsWriter,
{
  writer: KVW,
  suffix: i32,
}

impl<KVW> Closeable for WriterAndSuffix<KVW>
where
  KVW: KnnVectorsWriter,
{
  fn close(&mut self) -> Result<()> {
    self.writer.close()
  }
}

pub struct FieldsWriter<B, KVW>
where
  B: PerFieldKnnVectorsFormatBase,
  KVW: KnnVectorsWriter,
{
  base: Arc<B>,
  formats: HashMap<Identity, WriterAndSuffix<KVW>>,
  suffixes: HashMap<String, i32>,
  field_writers: Vec<(Identity, usize)>,
}

impl<B, KVW> FieldsWriter<B, KVW>
where
  B: PerFieldKnnVectorsFormatBase,
  KVW: KnnVectorsWriter,
{
  fn new(base: Arc<B>) -> Self {
    Self {
      base,
      formats: HashMap::new(),
      suffixes: HashMap::new(),
      field_writers: Vec::new(),
    }
  }

  fn get_instance<D1, D2>(
    &mut self,
    write_state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
    field: &Arc<FieldInfo>,
  ) -> Result<(Identity, String, &mut KVW)>
  where
    D1: Directory<IndexOutput = KVW::IndexOutput>,
    D2: Directory,
    B::Format: KnnVectorsFormat<KnnVectorsWriter<KVW::IndexOutput> = KVW>,
  {
    let base = Arc::clone(&self.base);
    let format = base.get_knn_vectors_format_for_field(&field.name)?;
    let format_name = format.get_name().to_string();
    let identity = format.identity().clone();

    field.put_attribute(PER_FIELD_FORMAT_KEY.to_string(), format_name.clone());
    let suffix;

    if !self.formats.contains_key(&identity) {
      // First time we are seeing this format; create a new instance.
      suffix = *self
        .suffixes
        .entry(format_name.clone())
        .and_modify(|suffix| *suffix += 1)
        .or_insert(0);

      let segment_suffix = get_full_segment_suffix(
        &write_state.segment_suffix,
        &get_suffix(&format_name, suffix),
      );
      let state = SegmentWriteState::copy_with_suffix(write_state, segment_suffix);
      let writer = format.fields_writer(&state, segment_info)?;
      self
        .formats
        .insert(identity.clone(), WriterAndSuffix { writer, suffix });
    } else {
      // We've already seen this format, so just grab its suffix.
      if !self.suffixes.contains_key(&format_name) {
        return Err(LuceneError::illegal_state(format!(
          "no suffix for format name: {format_name}"
        )));
      }
      suffix = self
        .formats
        .get(&identity)
        .ok_or_else(|| {
          LuceneError::illegal_state(format!("missing vectors writer for field: {}", field.name))
        })?
        .suffix;
    }

    field.put_attribute(PER_FIELD_SUFFIX_KEY.to_string(), suffix.to_string());
    let segment_suffix = get_full_segment_suffix(
      &write_state.segment_suffix,
      &get_suffix(&format_name, suffix),
    );
    let writer = self.formats.get_mut(&identity).ok_or_else(|| {
      LuceneError::illegal_state(format!("missing vectors writer for field: {}", field.name))
    })?;
    Ok((identity, segment_suffix, &mut writer.writer))
  }
}

impl<B, KVW> KnnVectorsWriter for FieldsWriter<B, KVW>
where
  B: PerFieldKnnVectorsFormatBase,
  B::Format: KnnVectorsFormat<KnnVectorsWriter<KVW::IndexOutput> = KVW>,
  KVW: KnnVectorsWriter,
{
  type IndexOutput = KVW::IndexOutput;

  fn add_field<D1, D2>(
    &mut self,
    write_state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
    field_info: Arc<FieldInfo>,
  ) -> Result<usize>
  where
    D1: Directory<IndexOutput = Self::IndexOutput>,
    D2: Directory,
  {
    let (identity, delegate_index) = {
      let (identity, segment_suffix, writer) =
        self.get_instance(write_state, segment_info, &field_info)?;
      let state = SegmentWriteState::copy_with_suffix(write_state, segment_suffix);
      let delegate_index = writer.add_field(&state, segment_info, field_info)?;
      (identity, delegate_index)
    };
    self.field_writers.push((identity, delegate_index));
    Ok(self.field_writers.len() - 1)
  }

  fn flush<DM>(&mut self, max_doc: i32, sort_map: Option<&DM>) -> Result<()>
  where
    DM: DocMap,
  {
    for writer_and_suffix in self.formats.values_mut() {
      writer_and_suffix.writer.flush(max_doc, sort_map)?;
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
    D1: Directory,
    D2: Directory<IndexOutput = Self::IndexOutput>,
    CR: CodecReader,
  {
    let (_, segment_suffix, writer) =
      self.get_instance(segment_write_state, merge_state.segment_info, field_info)?;
    let state = SegmentWriteState::copy_with_suffix(segment_write_state, segment_suffix);
    writer.merge_one_field(field_info, merge_state, &state)
  }

  fn finish(&mut self) -> Result<()> {
    for writer_and_suffix in self.formats.values_mut() {
      writer_and_suffix.writer.finish()?;
    }
    Ok(())
  }

  fn add_value(
    &mut self,
    doc_id: i32,
    vector_value: &VectorValueEnum,
    field_vectors_writers_idx: usize,
  ) -> Result<()> {
    let (identity, delegate_index) = self
      .field_writers
      .get(field_vectors_writers_idx)
      .ok_or_else(|| {
        LuceneError::illegal_argument(format!(
          "invalid field vectors writer index: {field_vectors_writers_idx}"
        ))
      })?;
    self
      .formats
      .get_mut(identity)
      .ok_or_else(|| LuceneError::illegal_state("missing vectors writer"))?
      .writer
      .add_value(doc_id, vector_value, *delegate_index)
  }
}

impl<B, KVW> Accountable for FieldsWriter<B, KVW>
where
  B: PerFieldKnnVectorsFormatBase,
  KVW: KnnVectorsWriter,
{
  fn ram_bytes_used(&self) -> Result<i64> {
    let mut total = 0_i64;
    for writer_and_suffix in self.formats.values() {
      total = total.saturating_add(writer_and_suffix.writer.ram_bytes_used()?);
    }
    Ok(total)
  }
}

impl<B, KVW> Closeable for FieldsWriter<B, KVW>
where
  B: PerFieldKnnVectorsFormatBase,
  KVW: KnnVectorsWriter,
{
  fn close(&mut self) -> Result<()> {
    IOUtils::close(self.formats.values_mut(), Closeable::close)
  }
}

/// Vector reader that can wrap multiple delegate readers, selected by field.
pub struct FieldsReader<KVR>
where
  KVR: KnnVectorsReader,
{
  fields: HashMap<i32, Arc<KVR>>,
  field_infos: Arc<FieldInfos>,
}

impl<KVR> FieldsReader<KVR>
where
  KVR: KnnVectorsReader,
{
  fn new<PF, D1, D2>(
    read_state: &SegmentReadState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self>
  where
    PF: KnnVectorsFormat<KnnVectorsReader<D1::IndexInput> = KVR>,
    D1: Directory,
    D2: Directory,
  {
    let field_infos = Arc::clone(&read_state.field_infos);
    let mut fields = HashMap::new();
    let mut formats: HashMap<String, Arc<KVR>> = HashMap::new();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      // Read field name -> format name.
      for field_info in read_state.field_infos.iter() {
        if field_info.has_vector_values() {
          let field_name = &field_info.name;
          let Some(format_name) = field_info.get_attribute(PER_FIELD_FORMAT_KEY) else {
            // Null format name means the field is in field infos, but has no vectors.
            continue;
          };
          let suffix = field_info
            .get_attribute(PER_FIELD_SUFFIX_KEY)
            .ok_or_else(|| {
              LuceneError::illegal_state(format!(
                "missing attribute: {PER_FIELD_SUFFIX_KEY} for field: {field_name}"
              ))
            })?;
          let segment_suffix = get_full_segment_suffix(
            &read_state.segment_suffix,
            &get_suffix(&format_name, suffix),
          );
          if !formats.contains_key(&segment_suffix) {
            let format = PF::for_name(&format_name)?;
            let state = SegmentReadState::copy_with_suffix(read_state, &segment_suffix);
            let reader = Arc::new(format.fields_reader(&state, segment_info)?);
            formats.insert(segment_suffix.clone(), reader);
          }
          let reader = formats.get(&segment_suffix).ok_or_else(|| {
            LuceneError::illegal_state(format!("missing vectors reader for field: {field_name}"))
          })?;
          fields.insert(field_info.number, Arc::clone(reader));
        }
      }
      Ok(())
    }));

    match result {
      Ok(Ok(())) => {},
      Ok(Err(error)) => {
        IOUtils::close_while_handling_error(formats.values(), |format| format.close())?;
        return Err(error);
      },
      Err(error) => {
        let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
          IOUtils::close_while_handling_error(formats.values(), |format| format.close())
        }));
        match close_result {
          Ok(Ok(())) => std::panic::resume_unwind(error),
          Ok(Err(close_error)) => return Err(close_error),
          Err(close_error) => std::panic::resume_unwind(close_error),
        }
      },
    }

    Ok(Self {
      fields,
      field_infos,
    })
  }

  // Clone for merge.
  fn from_other(other: &Self) -> Result<Self> {
    let mut fields = HashMap::with_capacity(other.fields.len());
    for field_info in other.field_infos.iter() {
      if field_info.has_vector_values()
        && let Some(reader) = other.fields.get(&field_info.number)
      {
        let reader = match reader.as_ref().get_merge_instance()? {
          Some(reader) => Arc::new(reader),
          None => Arc::clone(reader),
        };
        fields.insert(field_info.number, reader);
      }
    }
    Ok(Self {
      fields,
      field_infos: Arc::clone(&other.field_infos),
    })
  }

  /// Returns the underlying vector reader for the given field.
  pub fn get_field_reader(&self, field: &str) -> Result<Option<&Arc<KVR>>> {
    let Some(field_info) = self.field_infos.field_info_by_name(field)? else {
      return Ok(None);
    };
    Ok(self.fields.get(&field_info.number))
  }
}

impl<KVR> HnswGraphProvider for FieldsReader<KVR>
where
  KVR: KnnVectorsReader,
{
  type HnswGraph = KVR::HnswGraph;

  fn is_hnsw_graph_provider(&self, field: &str) -> bool {
    self
      .get_field_reader(field)
      .ok()
      .flatten()
      .is_some_and(|reader| reader.is_hnsw_graph_provider(field))
  }

  fn get_graph(&self, field: &str) -> Result<Self::HnswGraph> {
    self
      .get_field_reader(field)?
      .ok_or_else(|| LuceneError::illegal_argument(format!("field=\"{field}\" not found")))?
      .get_graph(field)
  }
}

impl<KVR> KnnVectorsReader for FieldsReader<KVR>
where
  KVR: KnnVectorsReader,
{
  fn check_integrity(&self) -> Result<()> {
    for reader in self.fields.values() {
      reader.check_integrity()?;
    }
    Ok(())
  }

  type FloatVectorValues = KVR::FloatVectorValues;

  fn get_float_vector_values(&self, field: &str) -> Result<Self::FloatVectorValues> {
    self
      .get_field_reader(field)?
      .ok_or_else(|| LuceneError::illegal_argument(format!("field=\"{field}\" not found")))?
      .get_float_vector_values(field)
  }

  type ByteVectorValues = KVR::ByteVectorValues;

  fn get_byte_vector_values(&self, field: &str) -> Result<Self::ByteVectorValues> {
    self
      .get_field_reader(field)?
      .ok_or_else(|| LuceneError::illegal_argument(format!("field=\"{field}\" not found")))?
      .get_byte_vector_values(field)
  }

  fn get_quantization_state(&self, field: &str) -> Result<Option<ScalarQuantizer>> {
    match self.get_field_reader(field)? {
      Some(reader) => reader.get_quantization_state(field),
      None => Ok(None),
    }
  }

  fn is_flat_vectors_reader(&self, field: &str) -> bool {
    self
      .get_field_reader(field)
      .ok()
      .flatten()
      .is_some_and(|reader| reader.is_flat_vectors_reader(field))
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
    let Some(reader) = self.get_field_reader(field)? else {
      return Ok(());
    };
    reader.search_f32(field, target, knn_collector, accept_docs)
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
    let Some(reader) = self.get_field_reader(field)? else {
      return Ok(());
    };
    reader.search_u8(field, target, knn_collector, accept_docs)
  }

  fn get_merge_instance(&self) -> Result<Option<Self>>
  where
    Self: Sized,
  {
    Ok(Some(Self::from_other(self)?))
  }

  fn finish_merge(&self) -> Result<()> {
    for reader in self.fields.values() {
      reader.finish_merge()?;
    }
    Ok(())
  }
}

impl<KVR> CloseableRef for FieldsReader<KVR>
where
  KVR: KnnVectorsReader,
{
  fn close(&self) -> Result<()> {
    IOUtils::close(self.fields.values(), |reader| reader.close())
  }
}

fn get_suffix(format_name: &str, suffix: impl Display) -> String {
  format!("{format_name}_{suffix}")
}

fn get_full_segment_suffix(outer_segment_suffix: &str, segment_suffix: &str) -> String {
  if outer_segment_suffix.is_empty() {
    segment_suffix.to_string()
  } else {
    format!("{outer_segment_suffix}_{segment_suffix}")
  }
}

impl<B> KnnVectorsFormat for PerFieldKnnVectorsFormat<B>
where
  B: PerFieldKnnVectorsFormatBase,
{
  fn get_name(&self) -> &str {
    PER_FIELD_NAME
  }

  type KnnVectorsWriter<O: IndexOutput> =
    FieldsWriter<B, <B::Format as KnnVectorsFormat>::KnnVectorsWriter<O>>;

  fn fields_writer<D1, D2>(
    &self,
    _state: &SegmentWriteState<D1>,
    _segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::KnnVectorsWriter<D1::IndexOutput>>
  where
    D1: Directory,
    D2: Directory,
  {
    Ok(FieldsWriter::new(Arc::clone(&self.base)))
  }

  type KnnVectorsReader<I: IndexInput> =
    FieldsReader<<B::Format as KnnVectorsFormat>::KnnVectorsReader<I>>;

  fn fields_reader<D1, D2>(
    &self,
    state: &SegmentReadState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::KnnVectorsReader<D1::IndexInput>>
  where
    D1: Directory,
    D2: Directory,
  {
    FieldsReader::new::<B::Format, D1, D2>(state, segment_info)
  }

  fn get_max_dimensions(&self, field_name: &str) -> Result<usize> {
    self
      .base
      .get_knn_vectors_format_for_field(field_name)?
      .get_max_dimensions(field_name)
  }

  fn for_name(name: &str) -> Result<Arc<Self>> {
    Err(LuceneError::illegal_argument(format!(
      "Could not load vectors format named \"{name}\""
    )))
  }
}
