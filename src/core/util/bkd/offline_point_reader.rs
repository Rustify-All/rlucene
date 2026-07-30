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
use crate::core::store::buffered_checksum_index_input::BufferedChecksumIndexInput;
use crate::core::store::directory::Directory;
use crate::core::store::{DataInput, IOContext, IndexInput};
use crate::core::util::bit_util::BitUtil;
use crate::core::util::bkd::bkd_config::BKDConfig;
use crate::core::util::bkd::point_reader::PointReader;
use crate::core::util::bkd::point_value::{PointValue, PointValueEnum};
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, Result};

pub struct OfflinePointReader<I>
where
  I: IndexInput,
{
  count_left: usize,
  input: Option<I>,
  check_sum_input: Option<BufferedChecksumIndexInput<I>>,
  offset: usize,
  checked: bool,
  closed: bool,
  config: BKDConfig,
  points_in_buffer: usize,
  max_point_on_heap: usize,
  // File name we are reading
  name: String,
  pub(crate) point_value: PointValueEnum,
}

impl<I> OfflinePointReader<I>
where
  I: IndexInput,
{
  pub fn new<D>(
    config: BKDConfig,
    temp_dir: &D,
    temp_file_name: &str,
    start: usize,
    length: usize,
    reusable_buffer: Vec<u8>,
  ) -> Result<Self>
  where
    D: Directory<IndexInput = I>,
  {
    let bytes_per_doc = config.bytes_per_doc();
    let footer_length = CodecUtil::footer_length();
    let file_length = temp_dir.file_length(temp_file_name)?;
    if ((start + length) * bytes_per_doc + footer_length) > file_length {
      return Err(LuceneError::illegal_argument(format!(
        "requested slice is beyond the length of this file: start={} length={} bytesPerDoc={} fileLength={} tempFileName={}",
        start,
        length,
        config.bytes_per_doc(),
        file_length,
        temp_file_name
      )));
    }
    let reusable_buffer_len = reusable_buffer.len();
    if reusable_buffer_len < config.bytes_per_doc() {
      return Err(LuceneError::illegal_argument(format!(
        "Length of reusableBuffer must be bigger than {}",
        config.bytes_per_doc()
      )));
    }

    debug_assert!(reusable_buffer_len <= i32::MAX as usize);
    let max_point_on_heap = reusable_buffer_len / config.bytes_per_doc();
    let name = temp_file_name.to_string();
    let seek_fp = start * bytes_per_doc;
    let (check_sum_input, input) =
      if start == 0 && (length * bytes_per_doc == file_length - footer_length) {
        let mut check_sum_input = temp_dir.open_checksum_input(temp_file_name)?;
        IndexInput::seek(&mut check_sum_input, seek_fp)?;
        (Some(check_sum_input), None)
      } else {
        let mut input = temp_dir.open_input(temp_file_name, &IOContext::read_once_io_context()?)?;
        input.seek(seek_fp)?;
        (None, Some(input))
      };

    let count_left = length;
    let point_value = PointValueEnum::Offline(OfflinePointValue::new(&config, reusable_buffer));

    Ok(OfflinePointReader {
      count_left,
      input,
      check_sum_input,
      offset: 0,
      checked: false,
      closed: false,
      config,
      points_in_buffer: 0,
      max_point_on_heap,
      name,
      point_value,
    })
  }
}
impl<I> Closeable for OfflinePointReader<I>
where
  I: IndexInput,
{
  fn close(&mut self) -> Result<()> {
    if self.closed {
      return Ok(());
    }

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      if self.count_left == 0
        && let Some(check_sum_input) = self.check_sum_input.as_mut()
        && !self.checked
      {
        self.checked = true;
        CodecUtil::check_footer(check_sum_input)?;
      }
      Ok(())
    }));

    let close_result = if let Some(input) = self.input.take() {
      std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| input.close()))
    } else if let Some(check_sum_input) = self.check_sum_input.take() {
      std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check_sum_input.close()))
    } else {
      Ok(Ok(()))
    };
    self.closed = true;
    match close_result {
      Ok(Ok(())) => match result {
        Ok(result) => result,
        Err(payload) => std::panic::resume_unwind(payload),
      },
      Ok(Err(error)) => Err(error),
      Err(payload) => std::panic::resume_unwind(payload),
    }
  }
}
impl<I> PointReader for OfflinePointReader<I>
where
  I: IndexInput,
{
  fn next(&mut self) -> Result<bool> {
    let bytes_per_doc = self.config.bytes_per_doc();
    if self.points_in_buffer == 0 {
      if self.count_left == 0 {
        return Ok(false);
      }
      let read_len;
      if self.count_left > self.max_point_on_heap {
        read_len = self.max_point_on_heap * bytes_per_doc;
        match &mut self.point_value {
          PointValueEnum::Offline(offline) => {
            match (self.check_sum_input.as_mut(), self.input.as_mut()) {
              (Some(input), None) => {
                input.read_bytes(&mut offline.value[0..read_len], 0, read_len)?;
              },
              (None, Some(input)) => {
                input.read_bytes(&mut offline.value[0..read_len], 0, read_len)?;
              },
              _ => {
                return Err(LuceneError::illegal_state(
                  "invalid state: exactly one of check_sum_input and input must be Some",
                ));
              },
            }
          },
          _ => {
            return Err(LuceneError::illegal_argument(
              "PointValueEnum must be Offline",
            ));
          },
        }

        self.points_in_buffer = self.max_point_on_heap - 1;
        self.count_left -= self.max_point_on_heap;
      } else {
        read_len = self.count_left * bytes_per_doc;
        match &mut self.point_value {
          PointValueEnum::Offline(offline) => {
            match (self.check_sum_input.as_mut(), self.input.as_mut()) {
              (Some(check_sum_input), None) => {
                check_sum_input.read_bytes(&mut offline.value[0..read_len], 0, read_len)?;
              },
              (None, Some(input)) => {
                input.read_bytes(&mut offline.value[0..read_len], 0, read_len)?;
              },
              _ => {
                return Err(LuceneError::illegal_state(
                  "invalid state: exactly one of check_sum_input and input must be Some",
                ));
              },
            }
          },
          _ => {
            return Err(LuceneError::illegal_argument(
              "PointValueEnum must be Offline",
            ));
          },
        }
        self.points_in_buffer = self.count_left - 1;
        self.count_left = 0;
      }
      self.offset = 0;
    } else {
      self.points_in_buffer -= 1;
      self.offset += bytes_per_doc;
    }
    Ok(true)
  }

  fn point_value(&mut self) -> Result<&PointValueEnum> {
    match &mut self.point_value {
      PointValueEnum::Offline(offline) => {
        offline.set_offset(self.offset);
      },
      _ => {
        return Err(LuceneError::illegal_argument(
          "PointValueEnum must be Offline",
        ));
      },
    }
    Ok(&self.point_value)
  }
}
impl<I> Drop for OfflinePointReader<I>
where
  I: IndexInput,
{
  fn drop(&mut self) {
    let _ = self.close();
  }
}

/// Reusable implementation for a point value offline.
pub(crate) struct OfflinePointValue {
  pub(crate) offset: usize,
  pub(crate) value: Vec<u8>,
  pub(crate) packed_value_length: usize,
  pub(crate) packed_value_doc_id_length: usize,
}
impl OfflinePointValue {
  pub fn new(config: &BKDConfig, value: Vec<u8>) -> Self {
    Self {
      offset: 0,
      value,
      packed_value_length: config.packed_bytes_length(),
      packed_value_doc_id_length: config.bytes_per_doc(),
    }
  }
}
impl PointValue for OfflinePointValue {
  fn set_offset(&mut self, offset: usize) {
    self.offset = offset;
  }

  fn packed_value(&self) -> (&[u8], usize, usize) {
    (&self.value, self.offset, self.packed_value_length)
  }

  fn doc_id(&self) -> i32 {
    let position = self.offset + self.packed_value_length;
    BitUtil::get_i32_be(&self.value[position..], 0)
  }

  fn packed_value_doc_id_bytes(&self) -> (&[u8], usize, usize) {
    (&self.value, self.offset, self.packed_value_doc_id_length)
  }
}
