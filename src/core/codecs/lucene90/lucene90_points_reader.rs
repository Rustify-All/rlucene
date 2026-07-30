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
use crate::core::index::IndexFileNames;
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
    D2: Directory,
  {
    let suffix = &read_state.segment_suffix;

    let meta_file_name = IndexFileNames::segment_file_name(
      &segment_info.name,
      suffix,
      Lucene90PointsFormat::META_EXTENSION,
    );
    let index_file_name = IndexFileNames::segment_file_name(
      &segment_info.name,
      suffix,
      Lucene90PointsFormat::INDEX_EXTENSION,
    );
    let data_file_name = IndexFileNames::segment_file_name(
      &segment_info.name,
      suffix,
      Lucene90PointsFormat::DATA_EXTENSION,
    );

    let mut index_in = Some(
      read_state.directory.open_input(
        &index_file_name,
        &read_state
          .context
          .with_read_advice_self(ReadAdvice::RandomPreload)?,
      )?,
    );
    let mut data_in = None;
    let setup_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
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
      data_in = Some(
        read_state.directory.open_input(
          &data_file_name,
          &read_state
            .context
            .with_read_advice_self(ReadAdvice::Normal)?,
        )?,
      );
      let data_in_ref = data_in
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("points data input is missing"))?;
      CodecUtil::check_index_header(
        data_in_ref,
        Lucene90PointsFormat::DATA_CODEC_NAME,
        Lucene90PointsFormat::VERSION_START,
        Lucene90PointsFormat::VERSION_CURRENT,
        segment_info.get_id(),
        suffix,
      )?;
      CodecUtil::retrieve_checksum(data_in_ref).map(|_| ())
    }));
    if !matches!(setup_result, Ok(Ok(()))) {
      IOUtils::close_while_handling_error(0..2, |operation| match operation {
        0 => match index_in.as_ref() {
          Some(input) => input.close(),
          None => Ok(()),
        },
        1 => match data_in.as_ref() {
          Some(input) => input.close(),
          None => Ok(()),
        },
        _ => unreachable!(),
      })?;
      return match setup_result {
        Ok(Err(error)) => Err(error),
        Err(payload) => std::panic::resume_unwind(payload),
        Ok(Ok(())) => unreachable!(),
      };
    }

    let mut index_length: i64 = -1;
    let mut data_length: i64 = -1;
    let mut tmp_readers = HashMap::new();

    let data_in =
      Arc::new(Mutex::new(data_in.take().ok_or_else(|| {
        LuceneError::illegal_state("points data input is missing")
      })?));
    let mut shared_index_in = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<Self> {
      let mut meta_in = read_state.directory.open_checksum_input(&meta_file_name)?;

      let mut footer_attempted = false;
      let mut result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        let result: Result<()> = (|| {
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
            let reader = BKDReader::new(
              &mut meta_in,
              index_in
                .as_mut()
                .ok_or_else(|| LuceneError::illegal_state("points index input is missing"))?,
              data_in.clone(),
            )?;
            tmp_readers.insert(field_number, reader);
          }

          index_length = meta_in.read_long()?;
          data_length = meta_in.read_long()?;
          Ok(())
        })();
        footer_attempted = true;
        match result {
          Ok(()) => CodecUtil::check_footer(&mut meta_in).map(|_| ()),
          Err(error) => Err(CodecUtil::check_footer_with_error(&mut meta_in, error)),
        }
      }));
      let footer_error = if let Err(payload) = &result
        && !footer_attempted
      {
        let error =
          LuceneError::tragedy_from_panic("panic while reading points metadata", payload.as_ref());
        Some(CodecUtil::check_footer_with_error(&mut meta_in, error))
      } else {
        None
      };
      if let Some(error @ LuceneError::CorruptIndex(_)) = footer_error {
        result = Ok(Err(error));
      }
      let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| meta_in.close()));
      IOUtils::use_or_suppress_caught_result(result, close_result)?;

      CodecUtil::retrieve_checksum_with_expected(
        index_in
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("points index input is missing"))?,
        index_length as usize,
      )?;
      CodecUtil::retrieve_checksum_with_expected(&mut *data_in.lock(), data_length as usize)?;
      shared_index_in =
        Some(Arc::new(index_in.take().ok_or_else(|| {
          LuceneError::illegal_state("points index input is missing")
        })?));
      let mut readers = HashMap::new();
      for mut value in tmp_readers.drain() {
        value.1.init_index_in(
          shared_index_in
            .as_ref()
            .ok_or_else(|| LuceneError::illegal_state("points index input is missing"))?
            .clone(),
        )?;
        readers.insert(value.0, Arc::new(value.1));
      }
      Ok(Self {
        index_in: shared_index_in
          .as_ref()
          .ok_or_else(|| LuceneError::illegal_state("points index input is missing"))?
          .clone(),
        data_in: data_in.clone(),
        readers: RwLock::new(readers),
        field_infos: read_state.field_infos.clone(),
      })
    }));
    match result {
      Ok(result @ Ok(_)) => result,
      result => {
        IOUtils::close_while_handling_error(0..2, |operation| match operation {
          0 => match shared_index_in.as_ref() {
            Some(input) => input.close(),
            None => match index_in.as_ref() {
              Some(input) => input.close(),
              None => Ok(()),
            },
          },
          1 => data_in.lock().close(),
          _ => unreachable!(),
        })?;
        match result {
          Ok(result) => result,
          Err(payload) => std::panic::resume_unwind(payload),
        }
      },
    }
  }
}

impl<I> CloseableRef for Lucene90PointsReader<I>
where
  I: IndexInput,
{
  fn close(&self) -> Result<()> {
    let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      let data_in = self.data_in.lock();
      IOUtils::close_refs([self.index_in.as_ref(), &*data_in])
    }));
    // Free up heap:
    self.readers.write().clear();
    match close_result {
      Ok(result) => result,
      Err(payload) => std::panic::resume_unwind(payload),
    }
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
}
