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
use crate::core::codecs::knn_vectors_format::{DEFAULT_MAX_DIMENSIONS, KnnVectorsFormat};
use crate::core::codecs::knn_vectors_formats::KnnVectorsFormatsReader;
use crate::core::codecs::knn_vectors_reader::KnnVectorsReader;
use crate::core::codecs::knn_vectors_writer::KnnVectorsWriter;
use crate::core::codecs::lucene99::lucene99_hnsw_vectors_format::Lucene99HnswVectorsFormat;
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::index_reader::Identity;
use crate::core::index::knn_vector_values::KnnVectorValues;
use crate::core::index::merge_state::MergeState;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::sorter::DocMap;
use crate::core::index::vector_encoding::VectorEncoding;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::knn_collector::KnnCollector;
use crate::core::store::directory::Directory;
use crate::core::store::{IndexInput, IndexOutput};
use crate::core::util::HasIdentity;
use crate::core::util::accountable::Accountable;
use crate::core::util::bits::Bits;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::quantization::scalar_quantizer::ScalarQuantizer;
use crate::test_framework::core::util::test_util::TestUtil;
use std::fmt::{Display, Formatter};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, OnceLock};

/// Wraps the default `KnnVectorsFormat` and provides additional assertions.
pub struct AssertingKnnVectorsFormat {
  delegate: Lucene99HnswVectorsFormat,
  identity: Identity,
}

impl AssertingKnnVectorsFormat {
  pub fn new() -> Result<Self> {
    Ok(Self {
      delegate: TestUtil::get_default_knn_vectors_format()?,
      identity: Identity::new(),
    })
  }
}

impl Display for AssertingKnnVectorsFormat {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "AssertingKnnVectorsFormat")
  }
}

impl HasIdentity for AssertingKnnVectorsFormat {
  fn identity(&self) -> &Identity {
    &self.identity
  }
}

impl KnnVectorsFormat for AssertingKnnVectorsFormat {
  fn get_name(&self) -> &str {
    "Asserting"
  }

  type KnnVectorsWriter<T: IndexOutput> =
    AssertingKnnVectorsWriter<<Lucene99HnswVectorsFormat as KnnVectorsFormat>::KnnVectorsWriter<T>>;

  fn fields_writer<D1, D2>(
    &self,
    state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::KnnVectorsWriter<D1::IndexOutput>>
  where
    D1: Directory,
  {
    Ok(AssertingKnnVectorsWriter::new(
      self.delegate.fields_writer(state, segment_info)?,
    ))
  }

  type KnnVectorsReader<T: IndexInput> = AssertingKnnVectorsReader<KnnVectorsFormatsReader<T>>;

  fn fields_reader<D1, D2>(
    &self,
    state: &SegmentReadState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::KnnVectorsReader<D1::IndexInput>>
  where
    D1: Directory,
  {
    Ok(AssertingKnnVectorsReader::new(
      KnnVectorsFormatsReader::Lucene99Hnsw(self.delegate.fields_reader(state, segment_info)?),
      state.field_infos.clone(),
    ))
  }

  fn get_max_dimensions(&self, _field_name: &str) -> Result<usize> {
    Ok(DEFAULT_MAX_DIMENSIONS)
  }

  fn for_name(name: &str) -> Result<Arc<Self>> {
    static FORMAT: OnceLock<Arc<AssertingKnnVectorsFormat>> = OnceLock::new();

    match name {
      "Asserting" => {
        if let Some(format) = FORMAT.get() {
          return Ok(Arc::clone(format));
        }
        let format = Arc::new(Self::new()?);
        if FORMAT.set(Arc::clone(&format)).is_ok() {
          Ok(format)
        } else {
          FORMAT.get().map(Arc::clone).ok_or_else(|| {
            LuceneError::illegal_state("failed to initialize vectors format named \"Asserting\"")
          })
        }
      },
      _ => Err(LuceneError::illegal_argument(format!(
        "Could not load vectors format named \"{name}\""
      ))),
    }
  }
}

pub struct AssertingKnnVectorsWriter<KVW> {
  delegate: KVW,
}

impl<KVW> AssertingKnnVectorsWriter<KVW> {
  fn new(delegate: KVW) -> Self {
    Self { delegate }
  }
}

impl<O, KVW> KnnVectorsWriter<O> for AssertingKnnVectorsWriter<KVW>
where
  O: IndexOutput,
  KVW: KnnVectorsWriter<O>,
{
  fn add_field<D1, D2>(
    &mut self,
    write_state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
    field_info: Arc<FieldInfo>,
  ) -> Result<usize>
  where
    D1: Directory<IndexOutput = O>,
  {
    self
      .delegate
      .add_field(write_state, segment_info, field_info)
  }

  fn flush<DM>(&mut self, max_doc: i32, sort_map: Option<&DM>) -> Result<()>
  where
    DM: DocMap,
  {
    self.delegate.flush(max_doc, sort_map)
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
      .delegate
      .merge_one_field(field_info, merge_state, segment_write_state)
  }

  fn finish(&mut self) -> Result<()> {
    self.delegate.finish()
  }

  fn add_value(
    &mut self,
    doc_id: i32,
    vector_value: &VectorValueEnum,
    field_vectors_writers_idx: usize,
  ) -> Result<()> {
    self
      .delegate
      .add_value(doc_id, vector_value, field_vectors_writers_idx)
  }
}

impl<KVW> Closeable for AssertingKnnVectorsWriter<KVW>
where
  KVW: Closeable,
{
  fn close(&mut self) -> Result<()> {
    self.delegate.close()
  }
}

impl<KVW> Accountable for AssertingKnnVectorsWriter<KVW>
where
  KVW: Accountable,
{
  fn ram_bytes_used(&self) -> Result<i64> {
    self.delegate.ram_bytes_used()
  }
}

pub struct AssertingKnnVectorsReader<KVR> {
  delegate: KVR,
  fis: Arc<FieldInfos>,
  merge_instance: bool,
  merge_instance_count: Arc<AtomicI32>,
  finish_merge_count: Arc<AtomicI32>,
  hook: AssertingKnnVectorsReaderHook,
}

enum AssertingKnnVectorsReaderHook {
  Default,
  MergeInstance {
    parent_finish_merge_count: Arc<AtomicI32>,
  },
}

struct AssertingKnnVectorsReaderDefaults;

trait AssertingKnnVectorsReaderBase<KVR>
where
  KVR: KnnVectorsReader,
{
  fn get_merge_instance(
    &self,
    reader: &AssertingKnnVectorsReader<KVR>,
  ) -> Result<Option<AssertingKnnVectorsReader<KVR>>> {
    AssertingKnnVectorsReaderDefaults::get_merge_instance(reader)
  }

  fn finish_merge(&self, reader: &AssertingKnnVectorsReader<KVR>) -> Result<()> {
    AssertingKnnVectorsReaderDefaults::finish_merge(reader)
  }

  fn close(&self, reader: &AssertingKnnVectorsReader<KVR>) -> Result<()> {
    AssertingKnnVectorsReaderDefaults::close(reader)
  }
}

impl AssertingKnnVectorsReaderDefaults {
  fn get_merge_instance<KVR>(
    reader: &AssertingKnnVectorsReader<KVR>,
  ) -> Result<Option<AssertingKnnVectorsReader<KVR>>>
  where
    KVR: KnnVectorsReader,
  {
    assert!(!reader.merge_instance);
    let merge_vectors_reader = reader
      .delegate
      .get_merge_instance()?
      .expect("the delegate must return a merge instance");
    reader.merge_instance_count.fetch_add(1, Ordering::SeqCst);

    let mut merge_reader = AssertingKnnVectorsReader::with_merge_instance(
      merge_vectors_reader,
      reader.fis.clone(),
      true,
    );
    merge_reader.hook = AssertingKnnVectorsReaderHook::MergeInstance {
      parent_finish_merge_count: reader.finish_merge_count.clone(),
    };
    Ok(Some(merge_reader))
  }

  fn finish_merge<KVR>(reader: &AssertingKnnVectorsReader<KVR>) -> Result<()>
  where
    KVR: KnnVectorsReader,
  {
    assert!(reader.merge_instance);
    reader.delegate.finish_merge()?;
    reader.finish_merge_count.fetch_add(1, Ordering::SeqCst);
    Ok(())
  }

  fn close<KVR>(reader: &AssertingKnnVectorsReader<KVR>) -> Result<()>
  where
    KVR: KnnVectorsReader,
  {
    assert!(!reader.merge_instance);
    reader.delegate.close()?;
    reader.delegate.close()?;
    let finish_merge_count = reader.finish_merge_count.load(Ordering::SeqCst);
    assert!(
      finish_merge_count <= 0
        || reader.merge_instance_count.load(Ordering::SeqCst) == finish_merge_count
    );
    Ok(())
  }
}

impl<KVR> AssertingKnnVectorsReaderBase<KVR> for AssertingKnnVectorsReaderHook
where
  KVR: KnnVectorsReader,
{
  #[allow(clippy::assertions_on_constants)]
  fn get_merge_instance(
    &self,
    reader: &AssertingKnnVectorsReader<KVR>,
  ) -> Result<Option<AssertingKnnVectorsReader<KVR>>> {
    match self {
      Self::Default => AssertingKnnVectorsReaderDefaults::get_merge_instance(reader),
      Self::MergeInstance { .. } => {
        assert!(false, "merging from a merge instance is not allowed");
        Ok(None)
      },
    }
  }

  fn finish_merge(&self, reader: &AssertingKnnVectorsReader<KVR>) -> Result<()> {
    match self {
      Self::Default => AssertingKnnVectorsReaderDefaults::finish_merge(reader),
      Self::MergeInstance {
        parent_finish_merge_count,
      } => {
        assert!(reader.merge_instance);
        reader.delegate.finish_merge()?;
        parent_finish_merge_count.fetch_add(1, Ordering::SeqCst);
        Ok(())
      },
    }
  }

  #[allow(clippy::assertions_on_constants)]
  fn close(&self, reader: &AssertingKnnVectorsReader<KVR>) -> Result<()> {
    match self {
      Self::Default => AssertingKnnVectorsReaderDefaults::close(reader),
      Self::MergeInstance { .. } => {
        assert!(false, "closing the merge instance is not allowed");
        Ok(())
      },
    }
  }
}

impl<KVR> AssertingKnnVectorsReader<KVR>
where
  KVR: KnnVectorsReader,
{
  fn new(delegate: KVR, field_infos: Arc<FieldInfos>) -> Self {
    Self::with_merge_instance(delegate, field_infos, false)
  }

  fn with_merge_instance(
    delegate: KVR,
    field_infos: Arc<FieldInfos>,
    merge_instance: bool,
  ) -> Self {
    Self {
      delegate,
      fis: field_infos,
      merge_instance,
      merge_instance_count: Arc::new(AtomicI32::new(0)),
      finish_merge_count: Arc::new(AtomicI32::new(0)),
      hook: AssertingKnnVectorsReaderHook::Default,
    }
  }
}

impl<KVR> HnswGraphProvider for AssertingKnnVectorsReader<KVR>
where
  KVR: KnnVectorsReader,
{
  type HnswGraph = KVR::HnswGraph;

  fn is_hnsw_graph_provider(&self, _field: &str) -> bool {
    true
  }

  fn get_graph(&self, field: &str) -> Result<Self::HnswGraph> {
    self.delegate.get_graph(field)
  }
}

impl<KVR> KnnVectorsReader for AssertingKnnVectorsReader<KVR>
where
  KVR: KnnVectorsReader,
{
  fn check_integrity(&self) -> Result<()> {
    self.delegate.check_integrity()
  }

  type FloatVectorValues = KVR::FloatVectorValues;

  fn get_float_vector_values(&self, field: &str) -> Result<Self::FloatVectorValues> {
    let field_info = self.fis.field_info_by_name(field)?;
    assert!(field_info.as_ref().is_some_and(|field_info| {
      field_info.get_vector_dimension() > 0
        && matches!(field_info.get_vector_encoding(), VectorEncoding::FLOAT32(_))
    }));
    let float_values = self.delegate.get_float_vector_values(field)?;
    assert_eq!(float_values.iterator()?.doc_id(), -1);
    assert!(float_values.dimension() > 0);
    let _ = float_values.size();
    Ok(float_values)
  }

  type ByteVectorValues = KVR::ByteVectorValues;

  fn get_byte_vector_values(&self, field: &str) -> Result<Self::ByteVectorValues> {
    let field_info = self.fis.field_info_by_name(field)?;
    assert!(field_info.as_ref().is_some_and(|field_info| {
      field_info.get_vector_dimension() > 0
        && matches!(field_info.get_vector_encoding(), VectorEncoding::BYTE(_))
    }));
    let values = self.delegate.get_byte_vector_values(field)?;
    assert_eq!(values.iterator()?.doc_id(), -1);
    assert!(values.dimension() > 0);
    let _ = values.size();
    Ok(values)
  }

  fn get_quantization_state(&self, _field: &str) -> Result<Option<ScalarQuantizer>> {
    Ok(None)
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
    assert!(!self.merge_instance);
    let field_info = self.fis.field_info_by_name(field)?;
    assert!(field_info.as_ref().is_some_and(|field_info| {
      field_info.get_vector_dimension() > 0
        && matches!(field_info.get_vector_encoding(), VectorEncoding::FLOAT32(_))
    }));
    self
      .delegate
      .search_f32(field, target, knn_collector, accept_docs)
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
    assert!(!self.merge_instance);
    let field_info = self.fis.field_info_by_name(field)?;
    assert!(field_info.as_ref().is_some_and(|field_info| {
      field_info.get_vector_dimension() > 0
        && matches!(field_info.get_vector_encoding(), VectorEncoding::BYTE(_))
    }));
    self
      .delegate
      .search_u8(field, target, knn_collector, accept_docs)
  }

  fn get_merge_instance(&self) -> Result<Option<Self>>
  where
    Self: Sized,
  {
    self.hook.get_merge_instance(self)
  }

  fn finish_merge(&self) -> Result<()> {
    self.hook.finish_merge(self)
  }
}

impl<KVR> CloseableRef for AssertingKnnVectorsReader<KVR>
where
  KVR: KnnVectorsReader,
{
  fn close(&self) -> Result<()> {
    self.hook.close(self)
  }
}
