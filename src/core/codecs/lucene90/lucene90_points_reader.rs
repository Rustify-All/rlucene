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
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::store::directory::Directory;
use crate::core::store::{DataInput, IndexInput, ReadAdvice};
use crate::core::util::bkd::bkd_reader::BKDReader;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::io_utils::IOUtils;
use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::sync::Arc;
/// Reads point values previously written with [`Lucene90PointsWriter`](crate::core::codecs::lucene90_points_writer::Lucene90PointsWriter)
pub struct Lucene90PointsReader<I>
where
  I: IndexInput,
{
  index_in: Arc<I>,
  data_in: Arc<Mutex<I>>,
  readers: RwLock<HashMap<i32, Arc<BKDReader<I>>>>,
  field_infos: Arc<FieldInfos>,
}

impl<I> Lucene90PointsReader<I>
where
  I: IndexInput,
{
  pub fn new<D1, D2>(
    read_state: &SegmentReadState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self>
  where
    D1: Directory<IndexInput = I>,
  {
    let suffix = &read_state.segment_suffix;

    let mut index_in = None;
    let mut shared_index_in = None;
    let mut data_in = None;
    let mut success = false;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<Self> {
      index_in = Some(read_state.with_file_name(
        &segment_info.name,
        Lucene90PointsFormat::INDEX_EXTENSION,
        |name| {
          read_state.directory.open_input(
            name,
            &read_state
              .context
              .with_read_advice_self(ReadAdvice::RandomPreload)?,
          )
        },
      )?);
      let index_in_ref = index_in
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("points index input is missing"))?;
      CodecUtil::check_index_header(
        index_in_ref,
        Lucene90PointsFormat::INDEX_CODEC_NAME,
        Lucene90PointsFormat::VERSION_START,
        Lucene90PointsFormat::VERSION_CURRENT,
        segment_info.get_id(),
        suffix,
      )?;
      CodecUtil::retrieve_checksum(index_in_ref)?;
      // Points read whole ranges of bytes at once, so pass ReadAdvice.NORMAL to perform readahead.
      data_in = Some(Arc::new(Mutex::new(read_state.with_file_name(
        &segment_info.name,
        Lucene90PointsFormat::DATA_EXTENSION,
        |name| {
          read_state.directory.open_input(
            name,
            &read_state
              .context
              .with_read_advice_self(ReadAdvice::Normal)?,
          )
        },
      )?)));
      let data_in_ref = data_in
        .as_ref()
        .ok_or_else(|| LuceneError::illegal_state("points data input is missing"))?;
      {
        let mut data_in_ref = data_in_ref.lock();
        CodecUtil::check_index_header(
          &mut *data_in_ref,
          Lucene90PointsFormat::DATA_CODEC_NAME,
          Lucene90PointsFormat::VERSION_START,
          Lucene90PointsFormat::VERSION_CURRENT,
          segment_info.get_id(),
          suffix,
        )?;
        CodecUtil::retrieve_checksum(&mut *data_in_ref)?;
      }

      let mut index_length: i64 = -1;
      let mut data_length: i64 = -1;
      let mut readers = HashMap::new();
      let mut meta_in = read_state.with_file_name(
        &segment_info.name,
        Lucene90PointsFormat::META_EXTENSION,
        |name| read_state.directory.open_checksum_input(name),
      )?;
      let metadata_result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
          let read_result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
              CodecUtil::check_index_header(
                &mut meta_in,
                Lucene90PointsFormat::META_CODEC_NAME,
                Lucene90PointsFormat::VERSION_START,
                Lucene90PointsFormat::VERSION_CURRENT,
                segment_info.get_id(),
                suffix,
              )?;

              loop {
                let field_number = meta_in.read_int()?;
                if field_number == -1 {
                  break;
                } else if field_number < 0 {
                  return Err(LuceneError::corrupt_index(format!(
                    "Illegal field number: {field_number}"
                  )));
                }
                let reader = BKDReader::read_metadata(
                  &mut meta_in,
                  index_in
                    .as_mut()
                    .ok_or_else(|| LuceneError::illegal_state("points index input is missing"))?,
                  data_in
                    .as_ref()
                    .ok_or_else(|| LuceneError::illegal_state("points data input is missing"))?
                    .clone(),
                )?;
                readers.insert(field_number, Arc::new(reader));
              }

              index_length = meta_in.read_long()?;
              data_length = meta_in.read_long()?;
              Ok(())
            }));
          let prior_result = match read_result {
            Ok(Ok(())) => None,
            read_result => Some(read_result),
          };
          CodecUtil::check_footer_with_error(&mut meta_in, prior_result)
        }));
      let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| meta_in.close()));
      IOUtils::use_or_suppress_caught_result(metadata_result, close_result)?;

      CodecUtil::retrieve_checksum_with_expected(
        index_in
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("points index input is missing"))?,
        index_length as usize,
      )?;
      CodecUtil::retrieve_checksum_with_expected(
        &mut *data_in
          .as_ref()
          .ok_or_else(|| LuceneError::illegal_state("points data input is missing"))?
          .lock(),
        data_length as usize,
      )?;
      shared_index_in =
        Some(Arc::new(index_in.take().ok_or_else(|| {
          LuceneError::illegal_state("points index input is missing")
        })?));
      for reader in readers.values_mut() {
        Arc::get_mut(reader)
          .ok_or_else(|| LuceneError::illegal_state("point reader is already shared"))?
          .init_index_in(
            shared_index_in
              .as_ref()
              .ok_or_else(|| LuceneError::illegal_state("points index input is missing"))?
              .clone(),
          )?;
      }
      let reader = Self {
        index_in: shared_index_in
          .take()
          .ok_or_else(|| LuceneError::illegal_state("points index input is missing"))?,
        data_in: data_in
          .take()
          .ok_or_else(|| LuceneError::illegal_state("points data input is missing"))?,
        readers: RwLock::new(readers),
        field_infos: read_state.field_infos.clone(),
      };
      success = true;
      Ok(reader)
    }));
    if !success {
      let index_in = shared_index_in.as_deref().or(index_in.as_ref());
      let data_in = data_in.as_ref().map(|input| input.lock());
      let data_in = data_in.as_deref();
      IOUtils::close_while_handling_exception((index_in, data_in));
    }
    unwrap_caught_result!(result)
  }
}

impl<I> CloseableRef for Lucene90PointsReader<I>
where
  I: IndexInput,
{
  fn close(&self) -> Result<()> {
    {
      let data_in = self.data_in.lock();
      IOUtils::close([self.index_in.as_ref(), &*data_in])?;
    }
    // Free up heap:
    self.readers.write().clear();
    Ok(())
  }
}

impl<I> PointsReader for Lucene90PointsReader<I>
where
  I: IndexInput,
{
  fn check_integrity(&self) -> Result<()> {
    CodecUtil::checksum_entire_file(self.index_in.as_ref())?;
    CodecUtil::checksum_entire_file(&*self.data_in.lock())?;
    Ok(())
  }

  type PointValuesType = Arc<BKDReader<I>>;

  fn get_values(&self, field_name: &str) -> Result<Option<Self::PointValuesType>> {
    match self.field_infos.field_info_by_name(field_name)? {
      Some(field_info) => {
        if field_info.get_point_dimension_count() == 0 {
          return Err(LuceneError::illegal_state(format!(
            "field=: {} does not index point values",
            field_name
          )));
        }
        Ok(self.readers.read().get(&field_info.number).cloned())
      },
      None => Err(LuceneError::illegal_state(format!(
        "field=: {} is unrecognized",
        field_name
      ))),
    }
  }

  fn is_lucene90_points_reader(&self) -> bool {
    true
  }
}
