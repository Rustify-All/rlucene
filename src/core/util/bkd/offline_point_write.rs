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
use crate::core::store::directory::Directory;
use crate::core::store::{IOContext, IndexInput, IndexOutput};
use crate::core::util::bkd::bkd_config::BKDConfig;
use crate::core::util::bkd::offline_point_reader::OfflinePointReader;
use crate::core::util::bkd::point_value::{PointValue, PointValueEnum};
use crate::core::util::bkd::point_writer::PointWriter;
use crate::core::util::close::Closeable;
use crate::core::util::error::lucene_error::{LuceneError, Result};

/// Writes points to disk in a fixed-width format.
pub struct OfflinePointWriter<O>
where
  O: IndexOutput,
{
  pub out: Option<O>,
  pub name: String,
  pub config: BKDConfig,
  pub count: usize,
  pub closed: bool,
  close_attempted: bool,
  pub expected_count: usize,
}

impl<O> OfflinePointWriter<O>
where
  O: IndexOutput,
{
  /// Create a new writer with an unknown number of incoming points
  pub fn new<D>(
    config: BKDConfig,
    temp_dir: &D,
    temp_file_name_prefix: &str,
    desc: &str,
    expected_count: usize,
  ) -> Result<Self>
  where
    D: Directory<IndexOutput = O>,
  {
    let out = temp_dir.create_temp_output(
      temp_file_name_prefix,
      &format!("bkd_{desc}"),
      &IOContext::default_io_context()?,
    )?;
    let name = out.get_name().to_string();
    Ok(OfflinePointWriter {
      out: Option::from(out),
      name,
      config,
      count: 0,
      closed: false,
      close_attempted: false,
      expected_count,
    })
  }

  pub fn get_reader_with_buffer<D>(
    &self,
    start: usize,
    length: usize,
    reusable_buffer: Vec<u8>,
    temp_dir: &D,
  ) -> Result<OfflinePointReader<D::IndexInput>>
  where
    D: Directory,
  {
    debug_assert!(
      self.closed,
      "point writer is still open and trying to get a reader"
    );
    debug_assert!(
      start + length <= self.count,
      "start={} length={} count={}",
      start,
      length,
      self.count
    );
    debug_assert!(
      self.expected_count == 0 || self.count == self.expected_count,
      "expectedCount={} vs count={}",
      self.expected_count,
      self.count
    );
    let reader = OfflinePointReader::new(
      self.config.clone(),
      temp_dir,
      &self.name,
      start,
      length,
      reusable_buffer,
    )?;
    Ok(reader)
  }
}
impl<O> Drop for OfflinePointWriter<O>
where
  O: IndexOutput,
{
  fn drop(&mut self) {
    if !self.close_attempted {
      let _ = self.close();
    }
  }
}

impl<O> std::fmt::Display for OfflinePointWriter<O>
where
  O: IndexOutput,
{
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(
      f,
      "{}(count={} tempFileName={})",
      std::any::type_name::<Self>(),
      self.count,
      self.name
    )
  }
}

impl<O> PointWriter for OfflinePointWriter<O>
where
  O: IndexOutput,
{
  fn append_bytes(&mut self, packed_value: &[u8], doc_id: i32) -> Result<()> {
    debug_assert!(!self.closed, "Point writer is already closed");
    debug_assert_eq!(
      packed_value.len(),
      self.config.packed_bytes_length(),
      "[packedValue] must have length [{}] but was [{}]",
      self.config.packed_bytes_length(),
      packed_value.len()
    );
    debug_assert!(packed_value.len() <= i32::MAX as usize);
    match self.out {
      None => return Err(LuceneError::illegal_state("Point writer is already closed")),
      Some(ref mut out) => {
        out.write_bytes_range(packed_value, 0, packed_value.len())?;
        out.write_int(i32::to_be(doc_id))?;
      },
    }
    self.count += 1;
    debug_assert!(
      self.expected_count == 0 || self.count <= self.expected_count,
      "expectedCount={} vs count={}",
      self.expected_count,
      self.count
    );
    Ok(())
  }

  fn append_point_value(&mut self, point_value: &PointValueEnum) -> Result<()> {
    debug_assert!(!self.closed, "Point writer is already closed");
    let (value, offset, length) = point_value.packed_value_doc_id_bytes();
    debug_assert_eq!(
      length,
      self.config.bytes_per_doc(),
      "[packedValue and docID] must have length [{}] but was [{}]",
      self.config.bytes_per_doc(),
      length
    );
    match self.out {
      None => return Err(LuceneError::illegal_state("Point writer is already closed")),
      Some(ref mut out) => {
        out.write_bytes_range(value, offset, length)?;
      },
    }
    self.count += 1;
    debug_assert!(
      self.expected_count == 0 || self.count <= self.expected_count,
      "expectedCount={} vs count={}",
      self.expected_count,
      self.count
    );
    Ok(())
  }

  type PointReader<I>
    = OfflinePointReader<I>
  where
    I: IndexInput;

  fn get_reader<D>(
    &mut self,
    start: usize,
    length: usize,
    temp_dir: &D,
  ) -> Result<Self::PointReader<D::IndexInput>>
  where
    D: Directory,
  {
    let buffer = vec![0u8; self.config.bytes_per_doc()];
    self.get_reader_with_buffer(start, length, buffer, temp_dir)
  }

  fn count(&self) -> usize {
    self.count
  }

  fn destroy<D>(&mut self, dir: &D) -> Result<()>
  where
    D: Directory,
  {
    dir.delete_file(&self.name)
  }
}
impl<O> Closeable for OfflinePointWriter<O>
where
  O: IndexOutput,
{
  fn close(&mut self) -> Result<()> {
    if !self.closed {
      self.close_attempted = true;
      if let Some(out) = self.out.as_mut() {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
          CodecUtil::write_footer(out)
        }));
        let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| out.close()));
        match close_result {
          Ok(Ok(())) => self.closed = true,
          Ok(Err(error)) => return Err(error),
          Err(payload) => std::panic::resume_unwind(payload),
        }
        return match result {
          Ok(result) => result,
          Err(payload) => std::panic::resume_unwind(payload),
        };
      }
      self.closed = true;
    }
    Ok(())
  }
}
