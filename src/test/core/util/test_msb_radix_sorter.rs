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
// Migrated from src/core/util/most_significant_bit_radix_sort.rs

use crate::test_framework::core::util::lucene_test_case::{at_least_usize, random};
use std::collections::HashSet;
use std::hash::{BuildHasherDefault, DefaultHasher};

use rand::Rng;
use rand::RngExt;

use crate::core::index::{BytesRef, BytesRefBuilder};
use crate::core::util::CoreHelper;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::{MSBRadixSorter, MSBRadixSorterBase, Sorter};
use crate::test::core::util::common_method::assert_vecs_equal;
use crate::test_framework::core::util::test_util::TestUtil;

#[allow(dead_code)] // for quick search
struct TestMSBRadixSorter;

fn test<R>(refs: &mut [BytesRef<Vec<u8>>], len: usize, random: &mut R) -> Result<()>
where
  R: Rng + ?Sized,
{
  let mut expected: Vec<BytesRef<Vec<u8>>> = refs[..len].to_vec();
  expected.sort();

  let mut max_length = 0;
  for ref_item in &refs[..len] {
    max_length = max_length.max(ref_item.length);
  }

  match random.random_range(0..3) {
    0 => max_length += TestUtil::next_usize(random, 1, 5),
    1 => max_length = i32::MAX as usize,
    _ => {},
  }

  let final_max_length = max_length;
  let delegate = MSBRadixSorterImpl::new(final_max_length, &mut refs[..len]);
  let mut msb_radix_sorter = MSBRadixSorter::new(max_length, delegate);
  msb_radix_sorter.sort(0, len)?;

  assert_vecs_equal(&expected, msb_radix_sorter.get_delegate().refs);
  Ok(())
}
#[test]
fn test_empty() -> Result<()> {
  let mut random = random();
  let mut refs: Vec<BytesRef<Vec<u8>>> = vec![BytesRef::default(); random.random_range(0..5)];
  test(&mut refs, 0, &mut random)
}
#[test]
fn test_one_value() -> Result<()> {
  let mut random = random();

  let bytes = BytesRef::from_string(&TestUtil::random_simple_string(&mut random));
  let mut refs = [bytes];
  test(&mut refs, 1, &mut random)
}
#[test]
fn test_two_values() -> Result<()> {
  let mut random = random();

  let bytes1 = BytesRef::from_string(&TestUtil::random_simple_string(&mut random));
  let bytes2 = BytesRef::from_string(&TestUtil::random_simple_string(&mut random));
  let mut refs = [bytes1, bytes2];

  test(&mut refs, 2, &mut random)
}

fn test_random_impl<R>(common_prefix_len: usize, max_len: usize, random: &mut R) -> Result<()>
where
  R: Rng + ?Sized,
{
  stack_or_heap_buffer!(common_prefix, u8, common_prefix_len, 30, 0);
  random.fill_bytes(common_prefix);
  let len = random.random_range(0..100_000);
  let mut bytes: Vec<BytesRef<Vec<u8>>> = Vec::with_capacity(len + random.random_range(0..50));
  for _ in 0..len {
    let mut b = vec![0u8; common_prefix_len + random.random_range(0..max_len)];
    random.fill_bytes(&mut b);

    b[..common_prefix.len()].copy_from_slice(common_prefix);

    bytes.push(BytesRef::from_bytes(b));
  }
  test(&mut bytes, len, random)
}
#[test]
fn test_random() -> Result<()> {
  let mut random = random();
  for _ in 0..10 {
    test_random_impl(0, 10, &mut random)?;
  }
  Ok(())
}

#[test]
fn test_random_with_lots_of_duplicates() -> Result<()> {
  let mut random = random();
  for _ in 0..10 {
    test_random_impl(0, 2, &mut random)?;
  }
  Ok(())
}

#[test]
fn test_random_with_shared_prefix() -> Result<()> {
  let mut random = random();
  for _ in 0..10 {
    let shared_prefix = TestUtil::next_usize(&mut random, 1, 30);
    test_random_impl(shared_prefix, 10, &mut random)?;
  }
  Ok(())
}

#[test]
fn test_random_with_shared_prefix_and_lots_of_duplicates() -> Result<()> {
  let mut random = random();
  for _ in 0..10 {
    let shared_prefix = TestUtil::next_usize(&mut random, 1, 30);
    test_random_impl(shared_prefix, 2, &mut random)?;
  }
  Ok(())
}

#[test]
fn test_random2() -> Result<()> {
  let mut random = random();
  // How large our alphabet is
  let letter_count = TestUtil::next_int(&mut random, 2, 10);

  // How many substring fragments to use
  let substring_count = TestUtil::next_usize(&mut random, 2, 10);
  let mut substrings_set = HashSet::<_, BuildHasherDefault<DefaultHasher>>::default();

  // How many strings to make
  let string_count = at_least_usize(&mut random, 10000);
  // let string_count = ;

  // Generate unique substrings
  while substrings_set.len() < substring_count {
    let length = TestUtil::next_int(&mut random, 2, 10);
    let bytes: Vec<u8> = (0..length)
      .map(|_| random.random_range(0..letter_count) as u8)
      .collect();
    let br = BytesRef::from_bytes(bytes);
    substrings_set.insert(br);
  }

  let substrings: Vec<BytesRef<Vec<u8>>> = Vec::from_iter(substrings_set);
  let mut chance = [0.0; 10];
  let chance = &mut chance[..substrings.len()];
  let mut sum = 0.0;

  for chance_value in chance.iter_mut() {
    *chance_value = random.random::<f64>();
    sum += *chance_value;
  }

  // give each substring a random chance of occurring:
  let mut accum = 0.0;
  for chance_value in chance.iter_mut() {
    accum += *chance_value / sum;
    *chance_value = accum;
  }

  // Generate unique strings
  let mut strings_set = HashSet::<_, BuildHasherDefault<DefaultHasher>>::default();
  let mut iters = 0;
  while strings_set.len() < string_count && iters < string_count * 5 {
    let count = random.random_range(1..=5);
    let mut builder = BytesRefBuilder::new();
    for _ in 0..count {
      let v = random.random::<f64>();
      let mut accum = 0.0;
      for (j, substring) in substrings.iter().enumerate() {
        accum += chance[j];
        if accum >= v {
          builder.append(substring)?;
          break;
        }
      }
    }
    CoreHelper::check_from_index_size(0, builder.length(), builder.bytes().bytes.len())?;
    let mut br = builder.get_bytes_owner();
    br.bytes.truncate(br.length);
    strings_set.insert(br);
    iters += 1;
  }

  // Run test with generated strings
  let mut strings: Vec<BytesRef<Vec<u8>>> = strings_set.into_iter().collect();
  let strings_len = strings.len();
  test(&mut strings, strings_len, &mut random)
}

pub struct MSBRadixSorterImpl<'a> {
  final_max_length: usize,
  refs: &'a mut [BytesRef<Vec<u8>>],
}

impl<'a> MSBRadixSorterImpl<'a> {
  fn new(final_max_length: usize, refs: &'a mut [BytesRef<Vec<u8>>]) -> Self {
    Self {
      final_max_length,
      refs,
    }
  }
}

impl MSBRadixSorterBase for MSBRadixSorterImpl<'_> {
  fn byte_at(&mut self, i: usize, k: usize) -> Result<i32> {
    assert!(
      k < self.final_max_length,
      "Index out of bounds: k={} exceeds final_max_length={}",
      k,
      self.final_max_length
    );

    let ref_item = &self.refs[i];
    if ref_item.length <= k {
      Ok(-1)
    } else {
      Ok(ref_item.bytes[ref_item.offset + k] as i32)
    }
  }
}
impl Sorter for MSBRadixSorterImpl<'_> {
  fn swap(&mut self, i: usize, j: usize) -> Result<()> {
    self.refs.swap(i, j);
    Ok(())
  }
}
