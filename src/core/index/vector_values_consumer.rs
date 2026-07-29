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
use crate::core::codecs::knn_vectors_format::{DefaultKnnVectorsWriter, KnnVectorsFormat};
use crate::core::codecs::knn_vectors_writer::KnnVectorsWriter;
use crate::core::codecs::{Codec, Codecs};
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::sorter::DocMap;
use crate::core::store::IOContext;
use crate::core::store::directory::Directory;
use crate::core::util::accountable::Accountable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::info_stream::InfoStreamMT;
use std::sync::Arc;
/// Streams vector values for indexing to the given codec's vectors writer.
/// The codec's vectors writer is responsible for buffering and processing vectors
pub(crate) struct VectorValuesConsumer<D>
where
  D: Directory,
{
  pub(crate) writer: Option<DefaultKnnVectorsWriter<D::IndexOutput>>,
  codec: Codecs,
  info_stream: InfoStreamMT,
  dir: D,
}
impl<D> VectorValuesConsumer<D>
where
  D: Directory,
{
  pub(crate) fn new(codec: Codecs, dir: D, info_stream: InfoStreamMT) -> Self {
    Self {
      writer: None,
      codec,
      info_stream,
      dir,
    }
  }
  fn init_knn_vectors_writer<D2>(&mut self, segment_info: &SegmentInfo<D2>) -> Result<()>
  where
    D2: Directory,
  {
    if self.writer.is_none() {
      let fmt = self.codec.knn_vectors_format()?;
      let context = IOContext::default_io_context()?;
      let padding_fi = Arc::new(FieldInfos::default());
      let initial_write_state =
        SegmentWriteState::new(self.info_stream.clone(), &self.dir, padding_fi, &context);
      self.writer = Some(fmt.fields_writer(&initial_write_state, segment_info)?);
    }
    Ok(())
  }
  pub(crate) fn add_field<D2>(
    &mut self,
    field_info: Arc<FieldInfo>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<usize>
  where
    D2: Directory,
  {
    self.init_knn_vectors_writer(segment_info)?;
    let context = IOContext::default_io_context()?;
    let padding_fi = Arc::new(FieldInfos::default());
    let write_state =
      SegmentWriteState::new(self.info_stream.clone(), &self.dir, padding_fi, &context);
    let writer = self
      .writer
      .as_mut()
      .ok_or_else(|| LuceneError::illegal_state("writer not initialized"))?;
    writer.add_field(&write_state, segment_info, field_info)
  }
  pub(crate) fn flush<DM, D2>(
    &mut self,
    segment_info: &mut SegmentInfo<D2>,
    sort_map: Option<&DM>,
  ) -> Result<()>
  where
    D2: Directory,
    DM: DocMap,
  {
    if let Some(mut writer) = self.writer.take() {
      writer.flush(segment_info.max_doc()?, sort_map)?;
      writer.finish()?;
    }
    Ok(())
  }
  pub(crate) fn abort(&mut self) {
    let _ = self.writer.take();
  }
  pub(crate) fn get_accountable(&self) -> &Self {
    self
  }
}

impl<D> Accountable for VectorValuesConsumer<D>
where
  D: Directory,
{
  fn ram_bytes_used(&self) -> Result<i64> {
    self
      .writer
      .as_ref()
      .map_or(Ok(0), Accountable::ram_bytes_used)
  }
}
