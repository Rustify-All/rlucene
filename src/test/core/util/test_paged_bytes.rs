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
use crate::core::store::IO_CONTEXT_DEFAULT;
// Migrated from src/core/util/paged_bytes.rs

use crate::test_framework::core::util::lucene_test_case::{
  at_least, create_temp_dir_with_prefix, is_night_mode, new_fs_directory, random,
};
use rand::RngExt;

use crate::core::index::BytesRef;
use crate::core::store::IndexOutput;
use crate::core::store::directory::{DirEnum, Directory};
use crate::core::store::{DataInput, DataOutput, IndexInput};
use crate::core::util::accountable::Accountable;
use crate::core::util::clone::TryClone;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::Result;
use crate::core::util::paged_bytes::{PagedBytes, get_data_input, get_data_output};
use crate::test_framework::core::store::mock_directory_wrapper::Throttling;
use crate::test_framework::core::util::test_util::TestUtil;
use std::mem;

#[allow(dead_code)] // for quick search
struct TestPagedBytes;
#[test]
fn test_data_input_output() -> Result<()> {
  let mut random = random();
  let num_iters = at_least(&mut random, 1);

  for _ in 0..num_iters {
    let dir = new_fs_directory(&mut random, create_temp_dir_with_prefix("testOverflow")?)?;
    if let DirEnum::B(mock) = dir.as_ref() {
      mock.set_throttling(Throttling::Never);
    }
    let block_bits = TestUtil::next_usize(&mut random, 1, 20);
    let block_size = 1 << block_bits;
    let mut paged_bytes = PagedBytes::new(block_bits);
    let mut out = dir.create_output("foo", IO_CONTEXT_DEFAULT.as_ref().map_err(Clone::clone)?)?;

    let num_bytes = if is_night_mode() {
      TestUtil::next_usize(&mut random, 2, 10_000_000)
    } else {
      TestUtil::next_usize(&mut random, 2, 1_000_000)
    };

    let mut answer = vec![0u8; num_bytes];
    random.fill(&mut answer[..]);

    {
      let mut written: usize = 0;
      while written < num_bytes {
        if random.random_range(0..100) == 7 {
          out.write_byte(answer[written])?;
          written += 1;
        } else {
          let chunk = std::cmp::min(random.random_range(0..1000), num_bytes - written);
          out.write_bytes_range(&answer, written, chunk)?;
          written += chunk;
        }
      }
    }

    out.close()?;
    let mut input = dir.open_input("foo", IO_CONTEXT_DEFAULT.as_ref().map_err(Clone::clone)?)?;
    let mut clone_input = input.try_clone()?;

    let len = input.length()?;
    paged_bytes.copy_with_input(&mut input, len)?;
    let reader = paged_bytes.freeze(random.random_bool(0.5))?;

    let mut verify = vec![0u8; num_bytes];
    let mut read = 0;
    while read < num_bytes {
      if random.random_range(0..100) == 7 {
        verify[read] = clone_input.read_byte()?;
        read += 1;
      } else {
        let chunk = std::cmp::min(random.random_range(0..1000), num_bytes - read);
        clone_input.read_bytes(&mut verify, read, chunk)?;
        read += chunk;
      }
    }

    assert_eq!(answer, verify);

    let mut slice = BytesRef::new();
    for _ in 0..100 {
      let pos = random.random_range(0..num_bytes - 1);
      assert_eq!(reader.get_byte(pos), answer[pos]);

      let len = random.random_range(0..std::cmp::min(block_size + 1, num_bytes - pos));
      let slice = reader.fill_slice(&mut slice, pos, len)?;

      for i in 0..len {
        assert_eq!(
          slice.bytes[slice.offset + i],
          answer[pos + i],
          "byte mismatch at pos {} + {}",
          pos,
          i
        );
      }
    }
    input.close()?;
    dir.close()?;
  }

  Ok(())
}
// Writes random bytes into `PagedBytes` via `get_data_output`, then verifies
// them with `get_data_input`.
#[test]
fn test_data_input_output_2() -> Result<()> {
  let mut random = random();
  let num_iters = at_least(&mut random, 1);

  for _ in 0..num_iters {
    let block_bits = TestUtil::next_usize(&mut random, 1, 20);
    let block_size = 1 << block_bits;
    let paged_bytes = PagedBytes::new(block_bits);
    let mut out = get_data_output(paged_bytes)?;

    let num_bytes = if is_night_mode() {
      TestUtil::next_usize(&mut random, 1, 10_000_000)
    } else {
      TestUtil::next_usize(&mut random, 1, 1_000_000)
    };

    let mut answer = vec![0u8; num_bytes];
    random.fill(&mut answer[..]);

    let mut written = 0;
    while written < num_bytes {
      if random.random_range(0..10) == 7 {
        out.write_byte(answer[written])?;
        written += 1;
      } else {
        let chunk = std::cmp::min(random.random_range(0..1000), num_bytes - written);
        out.write_bytes_range(&answer, written, chunk)?;
        written += chunk;
      }
    }

    let reader = out.paged_bytes.freeze(random.random_bool(0.5))?;
    let paged_bytes = std::mem::take(&mut out.paged_bytes);
    let mut input = get_data_input(&paged_bytes)?;

    let mut verify = vec![0u8; num_bytes];
    let mut read = 0;
    while read < num_bytes {
      if random.random_range(0..10) == 7 {
        verify[read] = input.read_byte()?;
        read += 1;
      } else {
        let chunk = std::cmp::min(random.random_range(0..1000), num_bytes - read);
        input.read_bytes(&mut verify, read, chunk)?;
        read += chunk;
      }
    }

    assert_eq!(answer, verify);

    let mut slice = BytesRef::new();
    for _ in 0..100 {
      let pos = random.random_range(0..num_bytes - 1);
      let len = random.random_range(0..std::cmp::min(block_size + 1, num_bytes - pos));
      let slice = reader.fill_slice(&mut slice, pos, len)?;
      for byte_upto in 0..len {
        assert_eq!(
          slice.bytes[slice.offset + byte_upto],
          answer[pos + byte_upto],
          "byte mismatch at pos {} + {}",
          pos,
          byte_upto
        );
      }
    }

    let mut input2 = get_data_input(&paged_bytes)?;
    let mut curr = 0;
    let max_skip_to = num_bytes - 1;
    while curr < max_skip_to {
      let skip_to = TestUtil::next_usize(&mut random, curr, max_skip_to);
      let step = skip_to - curr;
      input2.skip_bytes(step as i64)?;
      assert_eq!(answer[skip_to], input2.read_byte()?);
      curr = skip_to + 1;
    }
  }

  Ok(())
}
#[test]
#[ignore = "memory hole"]
fn test_overflow() -> Result<()> {
  let mut random = random();
  let dir = new_fs_directory(&mut random, create_temp_dir_with_prefix("testOverflow")?)?;
  if let DirEnum::B(mock) = dir.as_ref() {
    mock.set_throttling(Throttling::Never);
  }
  let block_bits = TestUtil::next_usize(&mut random, 14, 28);
  let block_size = 1 << block_bits;

  let arr_len = TestUtil::next_usize(&mut random, block_size / 2, block_size * 2);
  let mut arr = vec![0u8; arr_len];
  for (i, byte) in arr.iter_mut().enumerate().take(arr_len) {
    *byte = i as u8;
  }

  let extra = TestUtil::next_usize(&mut random, 1, block_size * 3);
  let num_bytes = (1 << 31) + extra;

  let mut paged_bytes = PagedBytes::new(block_bits);
  {
    let mut out = dir.create_output("foo", IO_CONTEXT_DEFAULT.as_ref().map_err(Clone::clone)?)?;

    let mut written = 0;
    while written < num_bytes {
      assert_eq!(written, out.get_file_pointer()?);
      let len = std::cmp::min(arr.len(), num_bytes - written);
      out.write_bytes_range(&arr, 0, len)?;
      written += len;
    }
    assert_eq!(num_bytes, out.get_file_pointer()?);
    out.close()?;
  }

  let mut input = dir.open_input("foo", IO_CONTEXT_DEFAULT.as_ref().map_err(Clone::clone)?)?;
  paged_bytes.copy_with_input(&mut input, num_bytes)?;
  let reader = paged_bytes.freeze(random.random_bool(0.5))?;

  let test_offsets = [
    0,
    i32::MAX as usize,
    num_bytes - 1,
    TestUtil::next_usize(&mut random, 1, num_bytes - 2),
  ];

  let mut b = BytesRef::new();
  for offset in test_offsets.into_iter() {
    let value = reader.fill_slice(&mut b, offset, 1)?;
    let expected = arr[offset % arr.len()];
    assert_eq!(
      expected, value.bytes[value.offset],
      "Mismatch at offset {}",
      offset
    );
  }
  input.close()?;
  dir.close()?;
  Ok(())
}
#[test]
fn test_ram_bytes_used() -> Result<()> {
  // TODO 未实现RamUsageTester
  let mut random = random();
  let block_bits = TestUtil::next_usize(&mut random, 4, 22);
  let block_size = 1usize << block_bits;
  let max_total_bytes = 10_000usize.min(block_size.saturating_mul(8).saturating_sub(12));
  let total_bytes = random.random_range(0..max_total_bytes);
  let mut untrimmed = PagedBytes::new(block_bits);
  let mut trimmed = PagedBytes::new(block_bits);

  assert_eq!(0, untrimmed.ram_bytes_used()?);
  assert_eq!(0, trimmed.ram_bytes_used()?);

  let mut pointer = 0;
  while pointer < total_bytes as i64 {
    let bytes = BytesRef::from_string(&TestUtil::random_simple_string_with_len(&mut random, 10));
    pointer = untrimmed.copy_using_length_prefix(&bytes)?;
    assert_eq!(pointer, trimmed.copy_using_length_prefix(&bytes)?);
  }

  let end_pointer = untrimmed.get_pointer();
  assert_eq!(end_pointer, trimmed.get_pointer());
  let end_pointer = end_pointer as usize;
  let allocated_blocks = if end_pointer == 0 {
    0
  } else {
    end_pointer.div_ceil(block_size)
  };
  // Before freeze, only completed pages occupy directory slots.
  let mut directory = Vec::<Vec<u8>>::new();
  for completed_blocks in 1..allocated_blocks {
    directory.resize_with(completed_blocks, Vec::new);
  }
  let directory_bytes = directory.capacity() * mem::size_of::<Vec<u8>>();
  let expected_bytes = (directory_bytes + allocated_blocks * block_size) as i64;
  assert_eq!(expected_bytes, untrimmed.ram_bytes_used()?);
  assert_eq!(expected_bytes, trimmed.ram_bytes_used()?);

  let untrimmed_reader = untrimmed.freeze(false)?;
  let trimmed_reader = trimmed.freeze(true)?;
  let unused_last_block = if end_pointer == 0 || end_pointer.is_multiple_of(block_size) {
    0
  } else {
    block_size - end_pointer % block_size
  } as i64;

  assert_eq!(
    unused_last_block,
    untrimmed.ram_bytes_used()? - trimmed.ram_bytes_used()?
  );
  assert_eq!(
    unused_last_block,
    untrimmed_reader.ram_bytes_used()? - trimmed_reader.ram_bytes_used()?
  );
  Ok(())
}
