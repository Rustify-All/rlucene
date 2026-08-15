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
use crate::test_framework::core::util::lucene_test_case::{new_directory, new_io_context, random};
use std::fmt::{Display, Formatter};

use rand::RngExt;

use crate::core::store::buffered_checksum_index_input::BufferedChecksumIndexInput;
use crate::core::store::check_sum_index_input::ChecksumIndexInput;
use crate::core::store::directory::Directory;
use crate::core::store::dummy::dummy_index_input::DummyIndexInput;
use crate::core::store::index_input::IndexInput;
use crate::core::store::{DataInput, DataOutput};
use crate::core::util::clone::TryClone;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::group_vint_util::GroupVIntUtil;
use crate::test_framework::core::util::test_util::TestUtil;
#[allow(dead_code)] // for quick search
pub struct TestChecksumIndexInput;

#[test]
fn test_skip_bytes() -> Result<()> {
  let mut random = random();
  let num_test_bytes = TestUtil::next_usize(&mut random, 100, 1000);
  let test_bytes = vec![0u8; num_test_bytes];
  let dir = new_directory(&mut random)?;
  {
    let mut os = dir.create_output("foo", &new_io_context(&mut random)?)?;
    os.write_bytes_with_len(&test_bytes, num_test_bytes)?;
    os.close()?;
  }

  let is = dir.open_input("foo", &new_io_context(&mut random)?)?;
  let mut checksum_index_input = InterceptingChecksumIndexInput::new(is, num_test_bytes);

  let mut skipped = 0;
  while skipped < num_test_bytes {
    let remaining = num_test_bytes - skipped;
    let step = if remaining < 10 {
      remaining
    } else {
      random.random_range(0..remaining)
    };
    DataInput::skip_bytes(&mut checksum_index_input, step as i64)?;
    skipped += step;
  }

  assert_eq!(test_bytes, checksum_index_input.read_bytes);
  checksum_index_input.close()?;
  dir.close()?;
  Ok(())
}

struct InterceptingChecksumIndexInput<T> {
  base: BufferedChecksumIndexInput<T>,
  read_bytes: Vec<u8>,
  off: usize,
}

impl<T> InterceptingChecksumIndexInput<T>
where
  T: IndexInput,
{
  fn new(main: T, len: usize) -> Self {
    Self {
      base: BufferedChecksumIndexInput::new(main),
      read_bytes: vec![0; len],
      off: 0,
    }
  }
}

impl<T> DataInput for InterceptingChecksumIndexInput<T>
where
  T: IndexInput,
{
  fn read_byte(&mut self) -> Result<u8> {
    self.base.read_byte()
  }

  fn read_bytes(&mut self, b: &mut [u8], offset: usize, len: usize) -> Result<()> {
    self.base.read_bytes(b, offset, len)?;
    self.read_bytes[self.off..self.off + len].copy_from_slice(&b[offset..offset + len]);
    self.off += len;
    Ok(())
  }

  fn read_group_vint(&mut self, dst: &mut [i32], offset: usize) -> Result<()> {
    GroupVIntUtil::read_group_vint_i32(self, dst, offset)
  }

  fn skip_bytes(&mut self, num_bytes: i64) -> Result<()> {
    IndexInput::skip_bytes(self, num_bytes)
  }

  fn is_index_input(&self) -> bool {
    true
  }

  fn seek_in_data_input(&mut self, pos: usize) -> Result<()> {
    debug_assert!(self.is_index_input());
    IndexInput::seek(self, pos)
  }

  fn get_file_pointer_in_data_input(&self) -> Result<usize> {
    debug_assert!(self.is_index_input());
    IndexInput::get_file_pointer(self)
  }
}

impl<T: IndexInput> CloseableRef for InterceptingChecksumIndexInput<T> {
  fn close(&self) -> Result<()> {
    self.base.close()
  }
}

impl<T> Display for InterceptingChecksumIndexInput<T>
where
  T: IndexInput,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "InterceptingChecksumIndexInput({})", self.base)
  }
}

impl<T> TryClone for InterceptingChecksumIndexInput<T>
where
  T: IndexInput,
{
  fn try_clone(&self) -> Result<Self>
  where
    Self: Sized,
  {
    Err(LuceneError::unsupported_operation(""))
  }
}

impl<T> IndexInput for InterceptingChecksumIndexInput<T>
where
  T: IndexInput,
{
  type IndexInput = InterceptingChecksumIndexInput<T>;

  fn get_file_pointer(&self) -> Result<usize> {
    self.base.get_file_pointer()
  }

  fn seek(&mut self, pos: usize) -> Result<()> {
    ChecksumIndexInput::seek(self, pos)
  }

  fn length(&self) -> Result<usize> {
    IndexInput::length(&self.base)
  }

  type RandomAccessSlice = DummyIndexInput;

  fn random_access_slice(&self, _offset: usize, _length: usize) -> Result<Self::RandomAccessSlice> {
    Err(LuceneError::unsupported_operation(
      "InterceptingChecksumIndexInput does not support random access slicing",
    ))
  }
}

impl<T> ChecksumIndexInput for InterceptingChecksumIndexInput<T>
where
  T: IndexInput,
{
  fn get_checksum(&mut self) -> i64 {
    self.base.get_checksum()
  }
}
