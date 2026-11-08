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
use crate::core::codecs::points_writer::{PointsWriter, default_merge};
use crate::core::index::field_info::FieldInfo;
use crate::core::index::merge_state::MergeState;
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
pub struct Lucene90PointsWriter<O> {
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
  {
    Self::new(
      write_state,
      BKDConfig::DEFAULT_MAX_POINTS_IN_LEAF_NODE,
      DEFAULT_MAX_MB_SORT_IN_HEAP as f64,
      segment_info,
    )
  }
  #[allow(clippy::too_many_arguments)]
  fn write_field_with_bytes<D1, D2, T, const N: usize>(
    &mut self,
    values: T,
    config: BKDConfig,
    max_doc: i32,
    total: i64,
    dir: &D1,
    segment_info: &SegmentInfo<D2>,
    field_info: &FieldInfo,
  ) -> Result<()>
  where
    D1: Directory,
    T: PointTree,
  {
    let mut writer = BKDWriter::<_, N>::new_with_shared_prefix(
      max_doc,
      dir,
      Arc::clone(&segment_info.name),
      config,
      self.max_mb_sort_in_heap,
      total,
    )?;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      match values.into_mutable() {
        PointTreeEnum::Mutable(mut mutable_tree) => {
          match writer.write_field(&mut self.data_out, &mut mutable_tree, &field_info.name)? {
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
      }
    }));
    let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| writer.close()));
    IOUtils::use_or_suppress_caught_result(result, close_result)
  }
  #[allow(clippy::too_many_arguments)]
  fn merge_with_bytes<D1, D2, CR, const N: usize>(
    &mut self,
    merge_state: &MergeState<D1, CR>,
    dir: &D2,
    field_info: &FieldInfo,
    config: BKDConfig,
    total_max_size: usize,
  ) -> Result<()>
  where
    D2: Directory,
    CR: crate::core::index::codec_reader::CodecReader,
  {
    let mut writer = BKDWriter::<_, N>::new_with_shared_prefix(
      merge_state.segment_info.max_doc()?,
      dir,
      Arc::clone(&merge_state.segment_info.name),
      config,
      self.max_mb_sort_in_heap,
      total_max_size.try_convert()?,
    )?;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      let mut point_values = Vec::new();
      let mut doc_maps = Vec::new();
      for (i, reader) in merge_state.points_readers.iter().enumerate() {
        let Some(reader) = reader.as_ref() else {
          continue;
        };
        let Some(reader_field_info) =
          merge_state.field_infos[i].field_info_by_name(&field_info.name)?
        else {
          continue;
        };
        if reader_field_info.get_point_dimension_count() == 0 {
          continue;
        }
        if let Some(values) = reader.get_values(&reader_field_info.name)? {
          if point_values.is_empty() {
            let reader_count = merge_state.points_readers.len();
            point_values.reserve_exact(reader_count);
            doc_maps.reserve_exact(reader_count);
          }
          point_values.push(values);
          doc_maps.push(&merge_state.doc_maps[i]);
        }
      }

      if let Some(finalizer) = writer.merge(&mut self.data_out, Some(doc_maps), point_values)? {
        self.meta_out.write_int(field_info.number)?;
        writer.write_index(&mut self.meta_out, Some(&mut self.index_out), &finalizer)?;
      }
      Ok(())
    }));
    let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| writer.close()));
    IOUtils::use_or_suppress_caught_result(result, close_result)
  }
  pub fn new<D1, D2>(
    write_state: &SegmentWriteState<D1>,
    max_points_in_leaf_node: usize,
    max_mb_sort_in_heap: f64,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self>
  where
    D1: Directory<IndexOutput = O>,
  {
    debug_assert!(write_state.field_infos.has_point_values());

    let mut data_out = Some(
      write_state
        .create_segment_output(&segment_info.name, Lucene90PointsFormat::DATA_EXTENSION)?,
    );
    let mut meta_out = None;
    let mut index_out = None;
    let mut success = false;
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

      meta_out = Some(
        write_state
          .create_segment_output(&segment_info.name, Lucene90PointsFormat::META_EXTENSION)?,
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

      index_out = Some(
        write_state
          .create_segment_output(&segment_info.name, Lucene90PointsFormat::INDEX_EXTENSION)?,
      );
      CodecUtil::write_index_header(
        index_out
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("points index output is missing"))?,
        Lucene90PointsFormat::INDEX_CODEC_NAME,
        Lucene90PointsFormat::VERSION_CURRENT,
        segment_info.get_id(),
        &write_state.segment_suffix,
      )?;
      success = true;
      Ok(())
    }));

    if !success {
      IOUtils::close_while_handling_exception((
        meta_out.as_mut(),
        index_out.as_mut(),
        data_out.as_mut(),
      ));
    }
    unwrap_caught_result!(result)?;
    let (data_out, meta_out, index_out) = match (data_out, meta_out, index_out) {
      (Some(data_out), Some(meta_out), Some(index_out)) => (data_out, meta_out, index_out),
      (mut data_out, mut meta_out, mut index_out) => {
        IOUtils::close_while_handling_exception((
          meta_out.as_mut(),
          index_out.as_mut(),
          data_out.as_mut(),
        ));
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
    IOUtils::close([&mut self.meta_out, &mut self.index_out, &mut self.data_out])
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
  {
    let values = reader
      .get_values(&field_info.name)?
      .ok_or_else(|| LuceneError::illegal_state("PointValues is None"))?
      .get_point_tree()?;
    let config = BKDConfig::new(
      field_info.get_point_dimension_count(),
      field_info.get_point_index_dimension_count(),
      field_info.get_point_num_bytes(),
      self.max_points_in_leaf_node,
    )?;
    let max_doc = segment_info.max_doc()?;
    let total = values.size()?.try_convert()?;
    match config.bytes_per_dim {
      4 => self.write_field_with_bytes::<_, _, _, 4>(
        values,
        config,
        max_doc,
        total,
        dir,
        segment_info,
        field_info,
      ),
      8 => self.write_field_with_bytes::<_, _, _, 8>(
        values,
        config,
        max_doc,
        total,
        dir,
        segment_info,
        field_info,
      ),
      _ => self.write_field_with_bytes::<_, _, _, 0>(
        values,
        config,
        max_doc,
        total,
        dir,
        segment_info,
        field_info,
      ),
    }
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

  fn merge<D1, D2, CR>(&mut self, merge_state: &MergeState<D1, CR>, dir: &D2) -> Result<()>
  where
    D2: Directory,
    CR: crate::core::index::codec_reader::CodecReader,
  {
    // Use the bulk path only when every entry is a non-null Lucene90PointsReader.
    // Sorted/wrapped or absent readers use the generic merge.
    if merge_state.points_readers.iter().any(|reader| {
      reader
        .as_ref()
        .is_none_or(|reader| !reader.is_lucene90_points_reader())
    }) {
      return default_merge(self, merge_state, dir);
    }

    for reader in merge_state.points_readers.iter().flatten() {
      reader.check_integrity()?;
    }

    for field_info in merge_state.merge_field_infos.iter() {
      if field_info.get_point_dimension_count() == 0 {
        continue;
      }
      if field_info.get_point_dimension_count() != 1 {
        self.merge_one_field(merge_state, field_info, dir)?;
        continue;
      }

      // Worst case total maximum size (if none of the points are deleted).
      let mut total_max_size = 0usize;
      for (i, reader) in merge_state.points_readers.iter().enumerate() {
        if let Some(reader) = reader.as_ref()
          && let Some(reader_field_info) =
            merge_state.field_infos[i].field_info_by_name(&field_info.name)?
          && reader_field_info.get_point_dimension_count() > 0
          && let Some(values) = reader.get_values(&field_info.name)?
        {
          total_max_size = total_max_size.saturating_add(values.size()?);
        }
      }

      let config = BKDConfig::new(
        field_info.get_point_dimension_count(),
        field_info.get_point_index_dimension_count(),
        field_info.get_point_num_bytes(),
        self.max_points_in_leaf_node,
      )?;
      match config.bytes_per_dim {
        4 => self.merge_with_bytes::<_, _, _, 4>(
          merge_state,
          dir,
          field_info,
          config,
          total_max_size,
        )?,
        8 => self.merge_with_bytes::<_, _, _, 8>(
          merge_state,
          dir,
          field_info,
          config,
          total_max_size,
        )?,
        _ => self.merge_with_bytes::<_, _, _, 0>(
          merge_state,
          dir,
          field_info,
          config,
          total_max_size,
        )?,
      }
    }

    self.finish()
  }
}

struct IntersectVisitorImpl<'a, D, const N: usize>
where
  D: Directory,
{
  writer: &'a mut BKDWriter<D, N>,
}
impl<'a, D, const N: usize> IntersectVisitorImpl<'a, D, N>
where
  D: Directory,
{
  pub fn new(writer: &'a mut BKDWriter<D, N>) -> Self {
    Self { writer }
  }
}
impl<'a, D, const N: usize> IntersectVisitor for IntersectVisitorImpl<'a, D, N>
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
