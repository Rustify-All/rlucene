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
use crate::core::codecs::lucene90_points_format::Lucene90PointsFormat;
use crate::core::codecs::points_reader::PointsReader;
use crate::core::codecs::points_writer::PointsWriter;
use crate::core::index::IndexFileNames;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::point_values::Relation::CellCrossesQuery;
use crate::core::index::point_values::{
  IntersectVisitor, PointTree, PointTreeEnum, PointValues, Relation,
};
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::store::IndexOutput;
use crate::core::store::directory::Directory;
use crate::core::util::bkd::bkd_config::BKDConfig;
use crate::core::util::bkd::bkd_writer::{BKDWriter, DEFAULT_MAX_MB_SORT_IN_HEAP};
use crate::core::util::close::Closeable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::{IOUtils, TryIntoInt};
use std::sync::Arc;

/// Writes dimensional values
pub struct Lucene90PointsWriter<O>
where
  O: IndexOutput,
{
  data_out: O,
  meta_out: O,
  index_out: O,
  max_points_in_leaf_node: usize,
  max_mb_sort_in_heap: f64,
  finish: bool,
}

impl<O> Lucene90PointsWriter<O>
where
  O: IndexOutput,
{
  /// Uses the default values for `max_points_in_leaf_node` (512)
  /// and `max_mb_sort_in_heap` (16.0).
  pub fn with_default_config<D1, D2>(
    write_state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self>
  where
    D1: Directory<IndexOutput = O>,
    D2: Directory,
  {
    Self::new(
      write_state,
      BKDConfig::DEFAULT_MAX_POINTS_IN_LEAF_NODE,
      DEFAULT_MAX_MB_SORT_IN_HEAP as f64,
      segment_info,
    )
  }
  pub fn new<D1, D2>(
    write_state: &SegmentWriteState<D1>,
    max_points_in_leaf_node: usize,
    max_mb_sort_in_heap: f64,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self>
  where
    D1: Directory<IndexOutput = O>,
    D2: Directory,
  {
    debug_assert!(write_state.field_infos.has_point_values());

    let data_file = IndexFileNames::segment_file_name(
      &segment_info.name,
      &write_state.segment_suffix,
      Lucene90PointsFormat::DATA_EXTENSION,
    );
    let mut data_out = Some(
      write_state
        .directory
        .create_output(&data_file, write_state.context)?,
    );
    let mut meta_out = None;
    let mut index_out = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      CodecUtil::write_index_header(
        data_out
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("points data output is missing"))?,
        Lucene90PointsFormat::DATA_CODEC_NAME,
        Lucene90PointsFormat::VERSION_CURRENT,
        segment_info.get_id(),
        &write_state.segment_suffix,
      )?;

      let meta_file = IndexFileNames::segment_file_name(
        &segment_info.name,
        &write_state.segment_suffix,
        Lucene90PointsFormat::META_EXTENSION,
      );
      meta_out = Some(
        write_state
          .directory
          .create_output(&meta_file, write_state.context)?,
      );
      CodecUtil::write_index_header(
        meta_out
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("points metadata output is missing"))?,
        Lucene90PointsFormat::META_CODEC_NAME,
        Lucene90PointsFormat::VERSION_CURRENT,
        segment_info.get_id(),
        &write_state.segment_suffix,
      )?;

      let index_file = IndexFileNames::segment_file_name(
        &segment_info.name,
        &write_state.segment_suffix,
        Lucene90PointsFormat::INDEX_EXTENSION,
      );
      index_out = Some(
        write_state
          .directory
          .create_output(&index_file, write_state.context)?,
      );
      CodecUtil::write_index_header(
        index_out
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("points index output is missing"))?,
        Lucene90PointsFormat::INDEX_CODEC_NAME,
        Lucene90PointsFormat::VERSION_CURRENT,
        segment_info.get_id(),
        &write_state.segment_suffix,
      )
    }));

    match result {
      Ok(Ok(())) => {},
      result => {
        IOUtils::close_resources_while_handling_error((
          meta_out.as_mut(),
          index_out.as_mut(),
          data_out.as_mut(),
        ))?;
        return match result {
          Ok(Err(error)) => Err(error),
          Err(payload) => std::panic::resume_unwind(payload),
          Ok(Ok(())) => Err(LuceneError::illegal_state(
            "points construction entered failure handling after success",
          )),
        };
      },
    }
    let (data_out, meta_out, index_out) = match (data_out, meta_out, index_out) {
      (Some(data_out), Some(meta_out), Some(index_out)) => (data_out, meta_out, index_out),
      (mut data_out, mut meta_out, mut index_out) => {
        IOUtils::close_resources_while_handling_error((
          meta_out.as_mut(),
          index_out.as_mut(),
          data_out.as_mut(),
        ))?;
        return Err(LuceneError::illegal_state(
          "points outputs are missing after successful construction",
        ));
      },
    };

    Ok(Self {
      data_out,
      meta_out,
      index_out,
      max_points_in_leaf_node,
      max_mb_sort_in_heap,
      finish: false,
    })
  }
}

impl<O> Closeable for Lucene90PointsWriter<O>
where
  O: IndexOutput,
{
  fn close(&mut self) -> Result<()> {
    IOUtils::close(
      [&mut self.meta_out, &mut self.index_out, &mut self.data_out],
      Closeable::close,
    )
  }
}

impl<O> PointsWriter for Lucene90PointsWriter<O>
where
  O: IndexOutput,
{
  fn write_field<PR, D1, D2>(
    &mut self,
    field_info: &Arc<FieldInfo>,
    reader: &mut PR,
    dir: &D1,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<()>
  where
    PR: PointsReader,
    D1: Directory,
    D2: Directory,
  {
    let mut values = reader
      .get_values(&field_info.name)?
      .ok_or_else(|| LuceneError::illegal_state("PointValues is None"))?
      .get_point_tree()?;
    let config = BKDConfig::new(
      field_info.get_point_dimension_count(),
      field_info.get_point_index_dimension_count(),
      field_info.get_point_num_bytes(),
      self.max_points_in_leaf_node,
    )?;
    let mut writer = BKDWriter::new(
      segment_info.max_doc()?,
      dir,
      &segment_info.name,
      config,
      self.max_mb_sort_in_heap,
      values.size()?.try_convert()?,
    )?;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match values {
      PointTreeEnum::Mutable(ref mut mutable_tree) => {
        match writer.write_field(&mut self.data_out, mutable_tree, &field_info.name)? {
          Some(finalizer) => {
            self.meta_out.write_int(field_info.number)?;
            writer.write_index(&mut self.meta_out, Some(&mut self.index_out), &finalizer)
          },
          None => Ok(()),
        }
      },
      PointTreeEnum::Other(mut tree) => {
        let mut intersect_visitor = IntersectVisitorImpl::new(&mut writer);
        tree.visit_doc_values(&mut intersect_visitor)?;
        match writer.finish(&mut self.data_out)? {
          Some(finalizer) => {
            self.meta_out.write_int(field_info.number)?;
            writer.write_index(&mut self.meta_out, Some(&mut self.index_out), &finalizer)
          },
          None => Ok(()),
        }
      },
    }));
    let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| writer.close()));
    IOUtils::use_or_suppress_caught_result(result, close_result)
  }

  fn finish(&mut self) -> Result<()> {
    if self.finish {
      return Err(LuceneError::illegal_state("already finished"));
    }
    self.finish = true;

    self.meta_out.write_int(-1)?;
    CodecUtil::write_footer(&mut self.index_out)?;
    CodecUtil::write_footer(&mut self.data_out)?;
    self
      .meta_out
      .write_long(self.index_out.get_file_pointer()? as i64)?;
    self
      .meta_out
      .write_long(self.data_out.get_file_pointer()? as i64)?;
    CodecUtil::write_footer(&mut self.meta_out)?;
    Ok(())
  }
}

struct IntersectVisitorImpl<'a, D>
where
  D: Directory,
{
  writer: &'a mut BKDWriter<D>,
}
impl<'a, D> IntersectVisitorImpl<'a, D>
where
  D: Directory,
{
  pub fn new(writer: &'a mut BKDWriter<D>) -> Self {
    Self { writer }
  }
}
impl<'a, D> IntersectVisitor for IntersectVisitorImpl<'a, D>
where
  D: Directory,
{
  fn visit(&mut self, _doc_id: i32) -> Result<()> {
    Err(LuceneError::illegal_state(""))
  }

  fn visit_with_packed_value(&mut self, doc_id: i32, packed_value: &[u8]) -> Result<()> {
    self.writer.add(packed_value, doc_id)
  }

  fn compare(&self, _min_packed_value: &[u8], _max_packed_value: &[u8]) -> Result<Relation> {
    Ok(CellCrossesQuery)
  }
}
