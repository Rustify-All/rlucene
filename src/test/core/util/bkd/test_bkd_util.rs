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
use crate::test_framework::core::util::lucene_test_case::random;
use rand::RngExt;

use crate::core::util::bit_util::BitUtil;
use crate::core::util::bkd::bkd_util::BKDUtil;
use crate::test_framework::core::util::test_util::TestUtil;

#[allow(dead_code)] // for quick search
struct TestBKDUtil;

#[test]
fn test_equals4() {
  let mut random = random();
  let a_offset = TestUtil::next_usize(&mut random, 0, 3);
  let b_offset = TestUtil::next_usize(&mut random, 0, 3);

  let mut a = [0u8; 7];
  let a = &mut a[..BitUtil::INT_BYTES + a_offset];
  let mut b = [0u8; 7];
  let b = &mut b[..BitUtil::INT_BYTES + b_offset];

  for i in 0..BitUtil::INT_BYTES {
    a[a_offset + i] = random.random();
  }
  let copy_source = &a[a_offset..a_offset + 4];
  b[b_offset..][..copy_source.len()].copy_from_slice(copy_source);

  assert!(BKDUtil::equals4(a, a_offset, b, b_offset));

  for i in 0..BitUtil::INT_BYTES {
    loop {
      let random_byte: u8 = random.random();
      if random_byte != a[a_offset + i] {
        b[b_offset + i] = random_byte;
        break;
      }
    }
    assert!(!BKDUtil::equals4(a, a_offset, b, b_offset));
    b[b_offset + i] = a[a_offset + i];
  }
}
#[test]
fn test_equals8() {
  let mut random = random();
  let a_offset = TestUtil::next_usize(&mut random, 0, 7);
  let b_offset = TestUtil::next_usize(&mut random, 0, 7);
  let mut a = [0u8; 15];
  let a = &mut a[..BitUtil::LONG_BYTES + a_offset];
  let mut b = [0u8; 15];
  let b = &mut b[..BitUtil::LONG_BYTES + b_offset];

  for i in 0..BitUtil::LONG_BYTES {
    a[a_offset + i] = random.random();
  }
  let copy_source = &a[a_offset..a_offset + 8];
  b[b_offset..][..copy_source.len()].copy_from_slice(copy_source);

  assert!(BKDUtil::equals8(a, a_offset, b, b_offset));

  for i in 0..BitUtil::LONG_BYTES {
    loop {
      let random_byte: u8 = random.random();
      if random_byte != a[a_offset + i] {
        b[b_offset + i] = random_byte;
        break;
      }
    }
    assert!(!BKDUtil::equals8(a, a_offset, b, b_offset));
    b[b_offset + i] = a[a_offset + i];
  }
}

#[test]
fn test_common_prefix_length4() {
  let mut random = random();
  let a_offset = TestUtil::next_usize(&mut random, 0, 3);
  let b_offset = TestUtil::next_usize(&mut random, 0, 3);
  let mut a = [0u8; 7];
  let a = &mut a[..BitUtil::INT_BYTES + a_offset];
  let mut b = [0u8; 7];
  let b = &mut b[..BitUtil::INT_BYTES + b_offset];

  for i in 0..BitUtil::INT_BYTES {
    a[a_offset + i] = random.random();
    loop {
      let random_byte: u8 = random.random();
      if random_byte != a[a_offset + i] {
        b[b_offset + i] = random_byte;
        break;
      }
    }
  }

  for i in 0..BitUtil::INT_BYTES {
    assert_eq!(
      i as i32,
      BKDUtil::common_prefix_length4(a, a_offset, b, b_offset)
    );
    b[b_offset + i] = a[a_offset + i];
  }
  assert_eq!(4, BKDUtil::common_prefix_length4(a, a_offset, b, b_offset));
}

#[test]
fn test_common_prefix_length8() {
  let mut random = random();
  let a_offset = TestUtil::next_usize(&mut random, 0, 7);
  let b_offset = TestUtil::next_usize(&mut random, 0, 7);
  let mut a = [0u8; 15];
  let a = &mut a[..BitUtil::LONG_BYTES + a_offset];
  let mut b = [0u8; 15];
  let b = &mut b[..BitUtil::LONG_BYTES + b_offset];

  for i in 0..BitUtil::LONG_BYTES {
    a[a_offset + i] = random.random();
    loop {
      let random_byte: u8 = random.random();
      if random_byte != a[a_offset + i] {
        b[b_offset + i] = random_byte;
        break;
      }
    }
  }

  for i in 0..BitUtil::LONG_BYTES {
    assert_eq!(
      i as i32,
      BKDUtil::common_prefix_length8(a, a_offset, b, b_offset)
    );
    b[b_offset + i] = a[a_offset + i];
  }
  assert_eq!(8, BKDUtil::common_prefix_length8(a, a_offset, b, b_offset));
}

#[test]
fn test_common_prefix_length_n() {
  let mut random = random();
  let num_bytes = TestUtil::next_usize(&mut random, 2, 16);
  let a_offset = TestUtil::next_usize(&mut random, 0, num_bytes - 1);
  let b_offset = TestUtil::next_usize(&mut random, 0, num_bytes - 1);
  let mut a = [0u8; 31];
  let a = &mut a[..num_bytes + a_offset];
  let mut b = [0u8; 31];
  let b = &mut b[..num_bytes + b_offset];

  for i in 0..num_bytes {
    a[a_offset + i] = random.random();
    loop {
      let random_byte: u8 = random.random();
      if random_byte != a[a_offset + i] {
        b[b_offset + i] = random_byte;
        break;
      }
    }
  }

  for i in 0..num_bytes {
    assert_eq!(
      i as i32,
      BKDUtil::common_prefix_length_n(a, a_offset, b, b_offset, num_bytes)
    );
    b[b_offset + i] = a[a_offset + i];
  }
  assert_eq!(
    num_bytes as i32,
    BKDUtil::common_prefix_length_n(a, a_offset, b, b_offset, num_bytes)
  );
}
