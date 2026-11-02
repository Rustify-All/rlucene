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

use crate::core::store::data_input_ext::DataInputExt;
use std::fmt::{Display, Formatter};

use crate::core::store::DataInput;
use crate::core::store::random_access_input::RandomAccessInput;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::fst_impl::fst::BytesReader;
use crate::core::util::group_vint_util::GroupVIntUtil;

/// Implements reverse read from a RandomAccessInput.
pub struct ReverseRandomAccessReader<R> {
  input: R,
  pos: i64,
}

impl<R> ReverseRandomAccessReader<R> {
  pub fn new(input: R) -> Self {
    Self { input, pos: 0 }
  }
}

impl<R> crate::core::util::close::Closeable for ReverseRandomAccessReader<R> {}

impl<R> DataInput for ReverseRandomAccessReader<R>
where
  R: RandomAccessInput,
{
  #[inline(always)]
  fn read_byte(&mut self) -> Result<u8> {
    debug_assert!(self.pos >= 0);
    let b = self.input.read_byte(self.pos as usize)?;
    self.pos -= 1;
    Ok(b)
  }

  #[inline]
  fn get_bytes(&mut self, len: usize) -> Result<std::borrow::Cow<'_, [u8]>> {
    let mut bytes = Vec::with_capacity(len);
    for _ in 0..len {
      bytes.push(self.read_byte()?);
    }
    Ok(std::borrow::Cow::Owned(bytes))
  }

  fn read_bytes(&mut self, b: &mut [u8], offset: usize, len: usize) -> Result<()> {
    let mut i = offset;
    let end = offset + len;
    while i < end {
      debug_assert!(self.pos >= 0);
      b[i] = self.input.read_byte(self.pos as usize)?;
      self.pos -= 1;
      i += 1;
    }
    Ok(())
  }

  fn read_group_vint(&mut self, dst: &mut [i32], offset: usize) -> Result<()> {
    GroupVIntUtil::read_group_vint_i32(self, dst, offset)
  }

  fn skip_bytes(&mut self, num_bytes: i64) -> Result<()> {
    self.pos -= num_bytes;
    Ok(())
  }
}

impl<R> DataInputExt for ReverseRandomAccessReader<R> where R: RandomAccessInput {}

impl<R> BytesReader for ReverseRandomAccessReader<R>
where
  R: RandomAccessInput,
{
  fn get_position(&self) -> i64 {
    debug_assert!(self.pos >= 0);
    self.pos
  }

  fn set_position(&mut self, pos: i64) {
    self.pos = pos;
  }
}

impl<R> Display for ReverseRandomAccessReader<R> {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}
