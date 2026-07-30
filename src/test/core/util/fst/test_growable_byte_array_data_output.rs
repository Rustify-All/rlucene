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
use crate::test_framework::core::util::lucene_test_case::{
  at_least, is_night_mode, new_directory_shared, random,
};
use rand::{Rng, RngExt};

use crate::core::store::directory::Directory;
use crate::core::store::output_stream_data_output::OutputStreamDataOutput;
use crate::core::store::{ByteArrayDataInput, DataOutput, IOContext};
use crate::core::util::SliceCopyOps;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::fst_impl::growable_byte_array_data_output::GrowableByteArrayDataOutput;
use crate::test_framework::core::util::test_util::TestUtil;

#[test]
fn test_random() -> Result<()> {
  let mut random = random();
  let iters = at_least(&mut random, 10);
  let max_bytes = if is_night_mode() { 200_000 } else { 20_000 };

  for iter in 0..iters {
    let num_bytes = TestUtil::next_usize(&mut random, 1, max_bytes);
    let mut expected = vec![0u8; num_bytes];
    let mut bytes = GrowableByteArrayDataOutput::new();

    if cfg!(feature = "test_log_verbose") {
      println!("TEST: iter={} num_bytes={}", iter, num_bytes);
    }

    let mut pos = 0;
    while pos < num_bytes {
      if cfg!(feature = "test_log_verbose") {
        println!("  cycle pos={}", pos);
      }

      match random.random_range(0..2) {
        0 => {
          // write single byte
          let b = random.random::<u8>();
          if cfg!(feature = "test_log_verbose") {
            println!("    write_byte b={}", b);
          }
          expected[pos] = b;
          bytes.write_byte(b)?;
          pos += 1;
        },
        1 => {
          // write byte array
          let max_len = std::cmp::min(num_bytes - pos, 100);
          let len = random.random_range(0..max_len) as usize;
          let mut temp = vec![0u8; len as usize];
          random.fill_bytes(&mut temp);
          if cfg!(feature = "test_log_verbose") {
            println!("    write_bytes len={}, bytes={:?}", len, temp);
          }
          expected.copy_from(&temp[0..temp.len()], pos);
          bytes.write_bytes_range(&temp, 0, len)?;
          pos += len;
        },
        _ => unreachable!(),
      }

      assert_eq!(pos, bytes.get_position());

      // maybe truncate
      if pos > 0 && random.random_range(0..50) == 17 {
        let len = TestUtil::next_usize(&mut random, 1, std::cmp::min(pos, 100));
        pos -= len;
        bytes.set_position(pos)?;
        for v in expected.iter_mut().skip(pos).take(len) {
          *v = 0;
        }
        if cfg!(feature = "test_log_verbose") {
          println!("    truncate len={} new_pos={}", len, pos);
        }
      }

      // maybe verify
      if pos > 0 && random.random_range(0..200) == 17 {
        verify(&bytes, &expected, pos)?;
      }
    }

    let bytes_to_verify = if random.random_bool(0.5) {
      if cfg!(feature = "test_log_verbose") {
        println!("TEST: save/load final bytes");
      }
      let dir = new_directory_shared(&mut random)?;
      {
        let mut out = dir.create_output("bytes", &IOContext::default_io_context()?)?;
        bytes.write_to_data_output(&mut out)?;
      }

      let mut in_ = dir.open_input("bytes", &IOContext::default_io_context()?)?;
      let mut bytes_to_verify = GrowableByteArrayDataOutput::new();
      bytes_to_verify.copy_bytes(&mut in_, num_bytes)?;
      bytes_to_verify
    } else {
      bytes
    };

    verify(&bytes_to_verify, &expected, num_bytes)?;
  }

  Ok(())
}

#[test]
fn test_copy_bytes_on_byte_store() -> Result<()> {
  let mut random = random();
  let mut bytes = vec![0u8; 1024 * 8 + 10];
  let mut bytes_out = vec![0u8; bytes.len()];
  random.fill_bytes(&mut bytes);

  let offset = TestUtil::next_usize(&mut random, 0, 100);
  let len = bytes.len() - offset;

  let mut input = ByteArrayDataInput::with_range(bytes.as_slice(), offset, len);
  let mut o = GrowableByteArrayDataOutput::new();

  o.copy_bytes(&mut input, len)?;
  o.write_to(0, &mut bytes_out, 0, len);

  let expected = &bytes[offset..(offset + len)];
  let actual = &bytes_out[..len];

  assert_eq!(actual, expected);
  Ok(())
}

#[allow(dead_code)] // for quick search
struct TestGrowableByteArrayDataOutput;
fn verify(bytes: &GrowableByteArrayDataOutput, expected: &[u8], total_length: usize) -> Result<()> {
  assert_eq!(bytes.get_position(), total_length);
  if total_length == 0 {
    return Ok(());
  }
  if cfg!(feature = "test_log_verbose") {
    println!("  verify...");
  }

  // First verify the whole thing in one blast:
  let mut buffer = Vec::new();
  let mut output = OutputStreamDataOutput::new(&mut buffer);
  bytes.write_to_data_output(&mut output)?;

  let data = output.os.unwrap().into_inner().unwrap();
  assert_eq!(data.len(), total_length);

  for i in 0..total_length {
    assert_eq!(expected[i], data[i], "byte @ index={}", i);
  }
  Ok(())
}
