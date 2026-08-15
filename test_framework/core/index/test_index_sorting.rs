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
use crate::core::codecs::Codec;
use crate::core::codecs::points_format::PointsFormat;
use crate::core::codecs::points_reader::PointsReader;
use crate::core::codecs::points_writer::PointsWriter;
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::merge_state::MergeState;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::store::directory::Directory;
use crate::core::store::{IndexInput, IndexOutput};
use crate::core::util::close::Closeable;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::util::test_util::{DefaultCodec, DefaultPointsFormat, TestUtil};
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

#[allow(dead_code)] // for quick search
pub struct TestIndexSorting;

#[derive(Clone)]
pub struct AssertingNeedsIndexSortCodec {
  delegate: DefaultCodec,
  pub(crate) needs_index_sort: Arc<AtomicBool>,
  pub(crate) num_calls: Arc<AtomicI32>,
}

impl Default for AssertingNeedsIndexSortCodec {
  fn default() -> Self {
    Self::new()
  }
}

impl AssertingNeedsIndexSortCodec {
  pub fn new() -> Self {
    Self {
      delegate: TestUtil::get_default_codec(),
      needs_index_sort: Arc::new(AtomicBool::new(false)),
      num_calls: Arc::new(AtomicI32::new(0)),
    }
  }
}

impl Codec for AssertingNeedsIndexSortCodec {
  type PostingsFormat = <DefaultCodec as Codec>::PostingsFormat;
  type DocValuesFormat = <DefaultCodec as Codec>::DocValuesFormat;
  type StoredFieldsFormat = <DefaultCodec as Codec>::StoredFieldsFormat;
  type TermVectorsFormat = <DefaultCodec as Codec>::TermVectorsFormat;
  type FieldInfosFormat = <DefaultCodec as Codec>::FieldInfosFormat;
  type SegmentInfoFormat = <DefaultCodec as Codec>::SegmentInfoFormat;
  type NormsFormat = <DefaultCodec as Codec>::NormsFormat;
  type LiveDocsFormat = <DefaultCodec as Codec>::LiveDocsFormat;
  type CompoundFormat = <DefaultCodec as Codec>::CompoundFormat;
  type PointsFormat = AssertingNeedsIndexSortPointsFormat;
  type KnnVectorsFormat = <DefaultCodec as Codec>::KnnVectorsFormat;

  fn postings_format(&self) -> Self::PostingsFormat {
    self.delegate.postings_format()
  }

  fn doc_values_format(&self) -> Self::DocValuesFormat {
    self.delegate.doc_values_format()
  }

  fn stored_fields_format(&self) -> Self::StoredFieldsFormat {
    self.delegate.stored_fields_format()
  }

  fn term_vectors_format(&self) -> Self::TermVectorsFormat {
    self.delegate.term_vectors_format()
  }

  fn field_infos_format(&self) -> Self::FieldInfosFormat {
    self.delegate.field_infos_format()
  }

  fn segment_info_format(&self) -> Self::SegmentInfoFormat {
    self.delegate.segment_info_format()
  }

  fn norms_format(&self) -> Self::NormsFormat {
    self.delegate.norms_format()
  }

  fn live_docs_format(&self) -> Self::LiveDocsFormat {
    self.delegate.live_docs_format()
  }

  fn compound_format(&self) -> Self::CompoundFormat {
    self.delegate.compound_format()
  }

  fn points_format(&self) -> Self::PointsFormat {
    AssertingNeedsIndexSortPointsFormat {
      in_: self.delegate.points_format(),
      needs_index_sort: Arc::clone(&self.needs_index_sort),
      num_calls: Arc::clone(&self.num_calls),
    }
  }

  fn knn_vectors_format(&self) -> Result<Self::KnnVectorsFormat> {
    self.delegate.knn_vectors_format()
  }

  fn get_name(&self) -> &str {
    self.delegate.get_name()
  }
}

impl Display for AssertingNeedsIndexSortCodec {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    Display::fmt(&self.delegate, f)
  }
}

pub struct AssertingNeedsIndexSortPointsFormat {
  in_: DefaultPointsFormat,
  needs_index_sort: Arc<AtomicBool>,
  num_calls: Arc<AtomicI32>,
}

impl PointsFormat for AssertingNeedsIndexSortPointsFormat {
  type PointsWriter<T: IndexOutput> =
    AssertingNeedsIndexSortPointsWriter<<DefaultPointsFormat as PointsFormat>::PointsWriter<T>>;

  fn fields_writer<D1, D2>(
    &self,
    state: &SegmentWriteState<D1>,
    info: &SegmentInfo<D2>,
  ) -> Result<Self::PointsWriter<D1::IndexOutput>>
  where
    D1: Directory,
  {
    Ok(AssertingNeedsIndexSortPointsWriter {
      writer: self.in_.fields_writer(state, info)?,
      needs_index_sort: Arc::clone(&self.needs_index_sort),
      num_calls: Arc::clone(&self.num_calls),
    })
  }

  type PointsReader<T: IndexInput> = <DefaultPointsFormat as PointsFormat>::PointsReader<T>;

  fn fields_reader<D1, D2>(
    &self,
    state: &SegmentReadState<D1>,
    info: &SegmentInfo<D2>,
  ) -> Result<Self::PointsReader<D1::IndexInput>>
  where
    D1: Directory,
  {
    self.in_.fields_reader(state, info)
  }
}

pub struct AssertingNeedsIndexSortPointsWriter<PW> {
  writer: PW,
  needs_index_sort: Arc<AtomicBool>,
  num_calls: Arc<AtomicI32>,
}

impl<PW> PointsWriter for AssertingNeedsIndexSortPointsWriter<PW>
where
  PW: PointsWriter,
{
  fn write_field<PR, D1, D2>(
    &mut self,
    field_info: &Arc<FieldInfo>,
    values: &mut PR,
    dir: &D1,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<()>
  where
    PR: PointsReader,
    D1: Directory,
  {
    self
      .writer
      .write_field(field_info, values, dir, segment_info)
  }

  fn finish(&mut self) -> Result<()> {
    self.writer.finish()
  }

  fn merge<D1, D2, CR>(&mut self, merge_state: &MergeState<D1, CR>, dir: &D2) -> Result<()>
  where
    D2: Directory,
    CR: CodecReader,
  {
    // For single segment merge we cannot infer if the segment is already sorted or not.
    if merge_state.doc_maps.len() > 1 {
      assert_eq!(
        self.needs_index_sort.load(Ordering::Relaxed),
        merge_state.needs_index_sort
      );
    }
    self.num_calls.fetch_add(1, Ordering::Relaxed);
    self.writer.merge(merge_state, dir)
  }
}

impl<PW> Closeable for AssertingNeedsIndexSortPointsWriter<PW>
where
  PW: PointsWriter,
{
  fn close(&mut self) -> Result<()> {
    self.writer.close()
  }
}
