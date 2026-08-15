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
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::merge_state::MergeState;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::store::directory::Directory;
use crate::core::store::{IndexInput, IndexOutput};
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::test_framework::core::codecs::asserting_codec::assert_thread;
use crate::test_framework::core::index::asserting_leaf_reader::AssertingPointValues;
use crate::test_framework::core::util::test_util::{DefaultPointsFormat, TestUtil};
use std::sync::Arc;
use std::thread::ThreadId;

/// Just like the default point format but with additional asserts.
pub struct AssertingPointsFormat<PF = DefaultPointsFormat> {
  in_: PF,
}

impl AssertingPointsFormat<DefaultPointsFormat> {
  /// Creates a new `AssertingPointsFormat`.
  pub fn new() -> Self {
    Self {
      in_: TestUtil::get_default_codec().points_format(),
    }
  }
}

impl<PF> AssertingPointsFormat<PF>
where
  PF: PointsFormat,
{
  /// Expert: Creates an `AssertingPointsFormat` with the provided format.
  ///
  /// This is only intended to pass special parameters for testing.
  // TODO: can we randomize this a cleaner way? e.g. stored fields and vectors do
  // this with a separate codec...
  pub fn new_with_format(in_: PF) -> Self {
    Self { in_ }
  }
}

impl<PF> PointsFormat for AssertingPointsFormat<PF>
where
  PF: PointsFormat,
{
  type PointsWriter<T: IndexOutput> = AssertingPointsWriter<PF::PointsWriter<T>>;

  fn fields_writer<D1, D2>(
    &self,
    state: &SegmentWriteState<D1>,
    info: &SegmentInfo<D2>,
  ) -> Result<Self::PointsWriter<D1::IndexOutput>>
  where
    D1: Directory,
  {
    Ok(AssertingPointsWriter::new(
      state,
      self.in_.fields_writer(state, info)?,
    ))
  }

  type PointsReader<T: IndexInput> = AssertingPointsReader<PF::PointsReader<T>>;

  fn fields_reader<D1, D2>(
    &self,
    state: &SegmentReadState<D1>,
    info: &SegmentInfo<D2>,
  ) -> Result<Self::PointsReader<D1::IndexInput>>
  where
    D1: Directory,
  {
    Ok(AssertingPointsReader::new(
      info.max_doc()?,
      self.in_.fields_reader(state, info)?,
      state.field_infos.clone(),
      false,
    ))
  }
}

pub struct AssertingPointsReader<PR> {
  in_: Arc<PR>,
  max_doc: i32,
  field_infos: Arc<FieldInfos>,
  merging: bool,
  creation_thread: ThreadId,
}

impl<PR> AssertingPointsReader<PR>
where
  PR: PointsReader,
{
  fn new(max_doc: i32, in_: PR, field_infos: Arc<FieldInfos>, merging: bool) -> Self {
    Self {
      in_: Arc::new(in_),
      max_doc,
      field_infos,
      merging,
      creation_thread: std::thread::current().id(),
    }
  }
}

impl<PR> CloseableRef for AssertingPointsReader<PR>
where
  PR: PointsReader,
{
  fn close(&self) -> Result<()> {
    self.in_.close()?;
    self.in_.close()
  }
}

impl<PR> PointsReader for AssertingPointsReader<PR>
where
  PR: PointsReader,
{
  fn check_integrity(&self) -> Result<()> {
    self.in_.check_integrity()
  }

  type PointValuesType = AssertingPointValues<PR::PointValuesType>;

  fn get_values(&self, field: &str) -> Result<Option<Self::PointValuesType>> {
    let field_info = self.field_infos.field_info_by_name(field)?;
    assert!(
      field_info
        .as_ref()
        .is_some_and(|field_info| field_info.get_point_dimension_count() > 0)
    );
    if self.merging {
      assert_thread("PointsReader", self.creation_thread);
    }
    self
      .in_
      .get_values(field)?
      .map(|values| AssertingPointValues::new(values, self.max_doc))
      .transpose()
  }

  fn get_merge_instance(&self) -> Result<Option<Self>>
  where
    Self: Sized,
  {
    let in_ = match self.in_.get_merge_instance()? {
      Some(in_) => in_,
      None => self.in_.clone(),
    };
    Ok(Some(Self {
      in_,
      max_doc: self.max_doc,
      field_infos: self.field_infos.clone(),
      merging: true,
      creation_thread: std::thread::current().id(),
    }))
  }
}

pub struct AssertingPointsWriter<PW> {
  in_: PW,
}

impl<PW> AssertingPointsWriter<PW>
where
  PW: PointsWriter,
{
  fn new<D>(_write_state: &SegmentWriteState<D>, in_: PW) -> Self
  where
    D: Directory,
  {
    Self { in_ }
  }
}

impl<PW> PointsWriter for AssertingPointsWriter<PW>
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
    if field_info.get_point_dimension_count() == 0 {
      return Err(LuceneError::illegal_argument(format!(
        "writing field=\"{}\" but pointDimensionalCount is 0",
        field_info.name
      )));
    }
    self.in_.write_field(field_info, values, dir, segment_info)
  }

  fn merge<D1, D2, CR>(&mut self, merge_state: &MergeState<D1, CR>, dir: &D2) -> Result<()>
  where
    D2: Directory,
    CR: CodecReader,
  {
    self.in_.merge(merge_state, dir)
  }

  fn finish(&mut self) -> Result<()> {
    self.in_.finish()
  }
}

impl<PW> Closeable for AssertingPointsWriter<PW>
where
  PW: PointsWriter,
{
  fn close(&mut self) -> Result<()> {
    self.in_.close()?;
    self.in_.close()
  }
}
