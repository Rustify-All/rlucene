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
use crate::test_framework::core::util::lucene_test_case::{at_least_usize, random};
use std::fmt::{Display, Formatter};
use std::io::{Cursor, Read, Seek};
use std::sync::LazyLock;

use rand::{Rng, RngExt};

use crate::core::store::{DataInput, InputStreamDataInput};
use crate::core::util::close::Closeable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::test_framework::core::util::test_util::TestUtil;

#[allow(dead_code)] // for quick search
struct TestInputStreamDataInput;

static RANDOM_DATA: LazyLock<Vec<u8>> = LazyLock::new(|| {
  let mut random = random();
  let mut random_data = vec![0u8; at_least_usize(&mut random, 100)];
  random.fill_bytes(&mut random_data);
  random_data
});

fn before() -> NoReadInputStreamDataInput<Cursor<&'static [u8]>> {
  NoReadInputStreamDataInput::new(Cursor::new(RANDOM_DATA.as_slice()))
}

#[test]
fn test_skip_bytes() -> Result<()> {
  let mut random = random();

  // not using the wrapped (NoReadInputStreamDataInput) here since we want to actually read and
  // verify
  let mut input = InputStreamDataInput::new(Cursor::new(RANDOM_DATA.as_slice()));
  let max_skip_to = RANDOM_DATA.len() - 1;
  // skip chunks of bytes until exhausted
  let mut curr = 0;
  while curr < max_skip_to {
    let skip_to = TestUtil::next_usize(&mut random, curr, max_skip_to);
    let step = skip_to - curr;
    input.skip_bytes(step as i64)?;
    assert_eq!(RANDOM_DATA[skip_to], input.read_byte()?);
    curr = skip_to + 1; // +1 for read byte
  }
  input.close()
}

#[test]
fn test_no_read_when_skipping() -> Result<()> {
  let mut random = random();
  let mut input = before();
  let max_skip_to = RANDOM_DATA.len() - 1;
  // skip chunks of bytes until exhausted
  let mut curr = 0;
  while curr < max_skip_to {
    let step = random.random_range(0..=max_skip_to - curr);
    input.skip_bytes(step as i64)?;
    curr += step;
  }
  input.close()
}

#[test]
fn test_full_skip() -> Result<()> {
  let mut input = before();
  input.skip_bytes(RANDOM_DATA.len() as i64)?;
  input.close()
}

#[test]
fn test_skip_off_end() -> Result<()> {
  let mut input = before();
  assert!(matches!(
    input.skip_bytes(RANDOM_DATA.len() as i64 + 1),
    Err(LuceneError::Eof(_))
  ));
  input.close()
}

/// Panics on byte reads to ensure `skip_bytes` does not invoke `read`.
struct NoReadInputStreamDataInput<R> {
  input: InputStreamDataInput<R>,
}

impl<R: Read + Seek> NoReadInputStreamDataInput<R> {
  fn new(is: R) -> Self {
    Self {
      input: InputStreamDataInput::new(is),
    }
  }
}

impl<R: Read + Seek> DataInput for NoReadInputStreamDataInput<R> {
  fn read_byte(&mut self) -> Result<u8> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn read_bytes(&mut self, _b: &mut [u8], _offset: usize, _len: usize) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn read_group_vint(&mut self, _dst: &mut [i32], _offset: usize) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn skip_bytes(&mut self, num_bytes: i64) -> Result<()> {
    self.input.skip_bytes(num_bytes)
  }
}

impl<R: Read + Seek> crate::core::util::close::Closeable for NoReadInputStreamDataInput<R> {}

impl<R: Read + Seek> Display for NoReadInputStreamDataInput<R> {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", self.input)
  }
}
