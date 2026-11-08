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

use crate::core::codecs::indexed_disi::{
  DEFAULT_DENSE_RANK_POWER, write_bitset_with_dense_rank_power,
};
use crate::core::codecs::lucene90_norms_format::Lucene90NormsFormat;
use crate::core::codecs::norms_consumer::NormsConsumer;
use crate::core::codecs::norms_producer::NormsProducer;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::store::IndexOutput;
use crate::core::store::directory::Directory;
use crate::core::util::IOUtils;
use crate::core::util::close::Closeable;
use crate::core::util::error::lucene_error::LuceneError;
use crate::core::util::error::lucene_error::Result;
use std::sync::Arc;

/// Writer for
/// [`Lucene90NormsFormat`](crate::core::codecs::lucene90::lucene90_norms_format).
pub struct Lucene90NormsConsumer<O>
where
  O: IndexOutput,
{
  pub data: O,
  pub meta: O,
  pub max_doc: i32,
  closed: bool,
}
impl<O: IndexOutput> Lucene90NormsConsumer<O> {
  pub fn new<D1, D2>(
    state: &SegmentWriteState<D1>,
    data_codec: &str,
    data_extension: &str,
    meta_codec: &str,
    meta_extension: &str,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self>
  where
    D1: Directory<IndexOutput = O>,
  {
    let mut data = None;
    let mut meta = None;
    let mut success = false;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<i32> {
      data = Some(state.create_segment_output(&segment_info.name, data_extension)?);
      CodecUtil::write_index_header(
        data
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("data output is missing"))?,
        data_codec,
        Lucene90NormsFormat::VERSION_CURRENT,
        segment_info.get_id(),
        &state.segment_suffix,
      )?;

      meta = Some(state.create_segment_output(&segment_info.name, meta_extension)?);
      CodecUtil::write_index_header(
        meta
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("metadata output is missing"))?,
        meta_codec,
        Lucene90NormsFormat::VERSION_CURRENT,
        segment_info.get_id(),
        &state.segment_suffix,
      )?;

      let max_doc = segment_info.max_doc()?;
      success = true;
      Ok(max_doc)
    }));

    if !success {
      let close_result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
          if let Some(meta) = meta.as_mut() {
            meta.write_int(-1)?;
            CodecUtil::write_footer(meta)?;
          }
          if let Some(data) = data.as_mut() {
            CodecUtil::write_footer(data)?;
          }
          Ok(())
        }));
      IOUtils::close_while_handling_exception((data.as_mut(), meta.as_mut()));
      resume_caught_panic!(close_result);
    }
    let max_doc = unwrap_caught_result!(result)?;
    let (data, meta) = match (data, meta) {
      (Some(data), Some(meta)) => (data, meta),
      (mut data, mut meta) => {
        IOUtils::close_while_handling_exception((data.as_mut(), meta.as_mut()));
        return Err(LuceneError::illegal_state(
          "norms outputs are missing after successful construction",
        ));
      },
    };

    Ok(Self {
      data,
      meta,
      max_doc,
      closed: false,
    })
  }
  pub fn close(&mut self) -> Result<()> {
    if !self.closed {
      self.closed = true;
      let mut success = false;
      let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        self.meta.write_int(-1)?;
        CodecUtil::write_footer(&mut self.meta)?;
        CodecUtil::write_footer(&mut self.data)?;
        success = true;
        Ok(())
      }));
      if success {
        IOUtils::close([&mut self.data, &mut self.meta])?;
      } else {
        IOUtils::close_while_handling_exception((&mut self.data, &mut self.meta));
      }
      unwrap_caught_result!(result)?;
    }
    Ok(())
  }
  fn num_bytes_per_value(&self, min: i64, max: i64) -> u8 {
    if min >= max {
      0
    } else if min >= i8::MIN as i64 && max <= i8::MAX as i64 {
      1
    } else if min >= i16::MIN as i64 && max <= i16::MAX as i64 {
      2
    } else if min >= i32::MIN as i64 && max <= i32::MAX as i64 {
      4
    } else {
      8
    }
  }
  fn write_values<T, IO>(values: &mut T, num_bytes_per_value: u8, out: &mut IO) -> Result<()>
  where
    T: NumericDocValues,
    IO: IndexOutput,
  {
    macro_rules! write_values {
      ($write:ident, $value:ty) => {
        while values.next_doc()? != NO_MORE_DOCS {
          let value = values.long_value()?;
          out.$write(value as $value)?;
        }
      };
    }
    match num_bytes_per_value {
      1 => write_values!(write_byte, u8),
      2 => write_values!(write_short, i16),
      4 => write_values!(write_int, i32),
      8 => write_values!(write_long, i64),
      _ => return Err(LuceneError::unreachable("invalid byte width")),
    }
    Ok(())
  }
}
impl<O> Drop for Lucene90NormsConsumer<O>
where
  O: IndexOutput,
{
  fn drop(&mut self) {
    let _ = self.close();
  }
}

impl<O> Closeable for Lucene90NormsConsumer<O>
where
  O: IndexOutput,
{
  fn close(&mut self) -> Result<()> {
    Lucene90NormsConsumer::close(self)
  }
}

impl<O> NormsConsumer for Lucene90NormsConsumer<O>
where
  O: IndexOutput,
{
  fn add_norms_field<T>(&mut self, field: &Arc<FieldInfo>, norms_producer: &mut T) -> Result<()>
  where
    T: NormsProducer,
  {
    let mut num_docs_with_value = 0;
    let mut min = i64::MAX;
    let mut max = i64::MIN;
    {
      let mut values = norms_producer.get_norms(field)?;

      while values.next_doc()? != NO_MORE_DOCS {
        num_docs_with_value += 1;
        let v = values.long_value()?;
        min = min.min(v);
        max = max.max(v);
      }
    }

    debug_assert!(num_docs_with_value <= self.max_doc);

    self.meta.write_int(field.number)?;

    if num_docs_with_value == 0 {
      self.meta.write_long(-2)?; // docsWithFieldOffset
      self.meta.write_long(0)?; // docsWithFieldLength
      self.meta.write_short(-1)?; // jumpTableEntryCount
      self.meta.write_byte(-1i8 as u8)?; // denseRankPower
    } else if num_docs_with_value == self.max_doc {
      self.meta.write_long(-1)?;
      self.meta.write_long(0)?;
      self.meta.write_short(-1)?;
      self.meta.write_byte(-1i8 as u8)?;
    } else {
      let offset = self.data.get_file_pointer()?;
      self.meta.write_long(offset as i64)?; // docsWithFieldOffset

      let jump_table_entry_count;
      {
        let mut values = norms_producer.get_norms(field)?;
        jump_table_entry_count = write_bitset_with_dense_rank_power(
          &mut values,
          &mut self.data,
          DEFAULT_DENSE_RANK_POWER,
        )?;
      }
      self
        .meta
        .write_long((self.data.get_file_pointer()? - offset) as i64)?; // docsWithFieldLength
      self.meta.write_short(jump_table_entry_count)?;
      self.meta.write_byte(DEFAULT_DENSE_RANK_POWER as u8)?;
    }

    self.meta.write_int(num_docs_with_value)?;
    let num_bytes_per_value = self.num_bytes_per_value(min, max);
    self.meta.write_byte(num_bytes_per_value)?;

    if num_bytes_per_value == 0 {
      self.meta.write_long(min)?;
    } else {
      self.meta.write_long(self.data.get_file_pointer()? as i64)?;
      let mut values = norms_producer.get_norms(field)?;
      Self::write_values(&mut values, num_bytes_per_value, &mut self.data)?;
    }

    Ok(())
  }
}
