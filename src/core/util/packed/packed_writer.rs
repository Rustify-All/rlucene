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
use crate::core::store::DataOutput;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::packed::bulk_operation::{BulkOperation, of};
use crate::core::util::packed::bulk_operation_packed_enum::BulkOperationPackedEnum;
use crate::core::util::packed::format_behavior::FormatBehavior;
use crate::core::util::packed::{Encoder, Format, PackedInts, Writer};

pub(crate) struct PackedWriter<'a, T> {
  finished: bool,
  format: Format,
  encoder: &'static BulkOperationPackedEnum,
  next_blocks: smallvec::SmallVec<[u8; 32]>,
  next_values: smallvec::SmallVec<[i64; 8]>,
  iterations: usize,
  off: usize,
  written: i32,
  value_count: i32,
  pub bits_per_value: i32,
  data_output: &'a mut T,
}
impl<'a, T> PackedWriter<'a, T>
where
  T: DataOutput,
{
  pub fn new(
    format: Format,
    data_output: &'a mut T,
    value_count: i32,
    bits_per_value: i32,
    mem: i32,
  ) -> Result<Self> {
    let encoder = of(format, bits_per_value)?;
    debug_assert!(value_count >= 0);
    let iterations = encoder.compute_iterations(value_count, mem) as usize;
    let next_blocks = smallvec::smallvec![0; iterations * Encoder::byte_block_count(encoder)];
    let next_values = smallvec::smallvec![0; iterations * Encoder::byte_value_count(encoder)];

    Ok(Self {
      finished: false,
      format,
      encoder,
      next_blocks,
      next_values,
      iterations,
      off: 0,
      written: 0,
      value_count,
      bits_per_value,
      data_output,
    })
  }
  fn flush(&mut self) -> Result<()> {
    self.encoder.encode_i64_to_u8(
      &self.next_values,
      0,
      &mut self.next_blocks,
      0,
      self.iterations,
    );
    let block_count =
      self
        .format
        .byte_count(PackedInts::VERSION_CURRENT, self.off, self.bits_per_value);

    debug_assert!(block_count <= i32::MAX as usize);
    self
      .data_output
      .write_bytes_with_len(&self.next_blocks[0..block_count], block_count)?;
    self.next_values.fill(0);
    self.off = 0;
    Ok(())
  }
}
impl<T> Writer for PackedWriter<'_, T>
where
  T: DataOutput,
{
  fn get_format(&self) -> &Format {
    &self.format
  }

  fn add(&mut self, v: i64) -> Result<()> {
    debug_assert!(
      PackedInts::unsigned_bits_required(v) <= self.bits_per_value,
      "Value exceeds allowed bits per value"
    );
    debug_assert!(!self.finished, "Cannot add values after finishing writing");
    if self.value_count != -1 && self.written >= self.value_count {
      return Err(LuceneError::eof("Writing past end of stream"));
    }
    self.next_values[self.off] = v;
    self.off += 1;
    if self.off == self.next_values.len() {
      self.flush()?;
    }
    self.written += 1;
    Ok(())
  }

  fn bits_per_values(&self) -> i32 {
    self.bits_per_value
  }

  fn finish(&mut self) -> Result<()> {
    debug_assert!(!self.finished, "Already finished");
    if self.value_count != -1 {
      while self.written < self.value_count {
        self.add(0)?;
      }
    }
    self.flush()?;
    self.finished = true;
    Ok(())
  }

  fn ord(&self) -> i32 {
    self.written - 1
  }
}
