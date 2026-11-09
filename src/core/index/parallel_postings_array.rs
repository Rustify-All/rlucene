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
use crate::core::index::index_options::IndexOptions;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::Result;

pub(crate) struct ParallelPostingsArray {
  // Each record is [term text start, current stream address, byte-pool start].
  pub(crate) starts: Vec<[i32; 3]>,
}

impl ParallelPostingsArray {
  pub(crate) const BYTES_PER_POSTING: usize = 3 * BitUtil::INT_BYTES;

  pub(crate) fn new(size: usize) -> Self {
    Self {
      starts: vec![[0; 3]; size],
    }
  }
}
impl PostingsArrayBase for ParallelPostingsArray {
  fn bytes_per_posting(&self) -> usize {
    Self::BYTES_PER_POSTING
  }
  fn copy_to(&mut self, new_size: usize) -> Result<()> {
    ArrayUtil::grow_exact(&mut self.starts, new_size, || [0; 3])?;
    Ok(())
  }
}

impl PostingsArray for ParallelPostingsArray {
  fn new_for_index(size: usize, _index_options: IndexOptions) -> Self {
    Self::new(size)
  }
  fn get_address_offset(&self) -> &[[i32; 3]] {
    self.starts.as_slice()
  }
  fn set_address_offset(&mut self, index: usize, value: i32) {
    self.starts[index][1] = value;
  }
  fn get_text_starts(&self) -> &[[i32; 3]] {
    self.starts.as_slice()
  }
  fn set_text_starts(&mut self, index: usize, value: i32) {
    self.starts[index][0] = value;
  }
  fn get_byte_starts(&self) -> &[[i32; 3]] {
    self.starts.as_slice()
  }
  fn set_byte_starts(&mut self, index: usize, value: i32) {
    self.starts[index][2] = value;
  }
}

pub(crate) trait PostingsArrayBase {
  fn bytes_per_posting(&self) -> usize;
  #[allow(dead_code)]
  fn new_instance(&self, _size: i32) {}
  fn copy_to(&mut self, new_size: usize) -> Result<()>;
}

/// Concrete postings-array behavior fixed when a field's `TermsHashPerField` is created.
/// The implementation is selected by the type parameter, not by a per-term match.
pub(crate) trait PostingsArray: PostingsArrayBase + Sized {
  fn new_for_index(size: usize, index_options: IndexOptions) -> Self;

  fn grow(&mut self) -> Result<()> {
    let new_size = ArrayUtil::oversize(self.get_size() + 1, self.bytes_per_posting())?;
    self.copy_to(new_size)
  }
  fn get_size(&self) -> usize {
    self.get_text_starts().len()
  }
  fn get_address_offset(&self) -> &[[i32; 3]];
  fn set_address_offset(&mut self, index: usize, value: i32);
  fn get_text_starts(&self) -> &[[i32; 3]];
  fn set_text_starts(&mut self, index: usize, value: i32);
  fn get_byte_starts(&self) -> &[[i32; 3]];
  fn set_byte_starts(&mut self, index: usize, value: i32);
}
