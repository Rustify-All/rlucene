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
// Migrated from src/core/util/stable_msb_radix_sorter.rs

use crate::test_framework::core::util::lucene_test_case::{at_least_usize, random};
use std::collections::HashSet;
use std::hash::{BuildHasherDefault, DefaultHasher};

use rand::{Rng, RngExt};

use crate::core::index::{BytesRef, BytesRefBuilder};
use crate::core::util::CoreHelper;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::stable_msb_radix_sorter::{StableMSBRadixSorter, StableMSBRadixSorterBase};
use crate::core::util::{MSBRadixSorter, MSBRadixSorterBase, Sorter};
use crate::test::core::util::common_method::assert_vecs_equal;
use crate::test_framework::core::util::test_util::TestUtil;

#[allow(dead_code)] // for quick search
struct TestStableMSBRadixSorter;

fn test<R>(refs: &[BytesRef<Vec<u8>>], len: usize, random: &mut R) -> Result<()>
where
  R: Rng + ?Sized,
{
  let mut expected: Vec<&BytesRef<Vec<u8>>> = refs[..len].iter().collect();
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
  let mut actual: Vec<&BytesRef<Vec<u8>>> = refs[..len].iter().collect();
  let delegate = StableMSBRadixSorterTestImpl::new(final_max_length, &mut actual);
  let stable_msb_radix_sorter = StableMSBRadixSorter::new(delegate, final_max_length);
  let mut msb_radix_sorter = MSBRadixSorter::new(max_length, stable_msb_radix_sorter);
  msb_radix_sorter.sort(0, len)?;

  assert_vecs_equal(&expected, &actual);
  // Equal keys must retain the same original instance at every index.
  for (expected, actual) in expected.iter().zip(&actual) {
    assert!(std::ptr::eq(*expected, *actual));
  }
  Ok(())
}
#[test]
fn test_empty() -> Result<()> {
  let mut random = random();
  let refs: Vec<BytesRef<Vec<u8>>> = vec![BytesRef::default(); random.random_range(0..5)];
  test(&refs, 0, &mut random)
}
#[test]
fn test_one_value() -> Result<()> {
  let mut random = random();
  let bytes = BytesRef::from_string(&TestUtil::random_simple_string(&mut random));
  let refs = vec![bytes];
  test(&refs, 1, &mut random)
}

#[test]
fn test_two_values() -> Result<()> {
  let mut random = random();
  let bytes1 = BytesRef::from_string(&TestUtil::random_simple_string(&mut random));
  let bytes2 = BytesRef::from_string(&TestUtil::random_simple_string(&mut random));
  let refs = vec![bytes1, bytes2];
  test(&refs, 2, &mut random)
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
  test(&bytes, len, random)
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
    let common_prefix_len = TestUtil::next_usize(&mut random, 1, 30);
    test_random_impl(common_prefix_len, 10, &mut random)?;
  }
  Ok(())
}

#[test]
fn test_random_with_shared_prefix_and_lots_of_duplicates() -> Result<()> {
  let mut random = random();
  for _ in 0..10 {
    let common_prefix_len = TestUtil::next_usize(&mut random, 1, 30);
    test_random_impl(common_prefix_len, 2, &mut random)?;
  }
  Ok(())
}

#[test]
fn test_random2() -> Result<()> {
  let mut random = random();
  // how large our alphabet is
  let letter_count = TestUtil::next_int(&mut random, 2, 10);

  // how many substring fragments to use
  let substring_count = TestUtil::next_usize(&mut random, 2, 10);
  let mut substrings_set = HashSet::<_, BuildHasherDefault<DefaultHasher>>::default();

  // how many strings to make
  let string_count = at_least_usize(&mut random, 10000);

  // Generate substring fragments
  while substrings_set.len() < substring_count {
    let length = TestUtil::next_usize(&mut random, 2, 10);
    let mut bytes = vec![0u8; length];
    for byte in &mut bytes {
      *byte = random.random_range(0..letter_count) as u8;
    }
    substrings_set.insert(BytesRef::from_bytes(bytes));
  }

  let substrings: Vec<BytesRef<Vec<u8>>> = substrings_set.into_iter().collect();
  let mut chance = [0.0; 10];
  let chance = &mut chance[..substrings.len()];
  let mut sum = 0.0;

  // Generate random chances
  for probability in chance.iter_mut() {
    let value = random.random::<f64>();
    *probability = value;
    sum += value;
  }

  // give each substring a random chance of occurring:
  let mut accum = 0.0;
  for value in chance.iter_mut() {
    accum += *value / sum;
    *value = accum;
  }

  let mut strings_set = HashSet::<_, BuildHasherDefault<DefaultHasher>>::default();
  let mut iters = 0;

  while strings_set.len() < string_count && iters < string_count * 5 {
    let count = TestUtil::next_int(&mut random, 1, 5);
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

  let strings_vec: Vec<BytesRef<Vec<u8>>> = strings_set.into_iter().collect();
  test(&strings_vec, strings_vec.len(), &mut random)
}

struct StableMSBRadixSorterTestImpl<'a, 'b> {
  temp: Vec<&'a BytesRef<Vec<u8>>>,
  final_max_length: usize,
  refs: &'b mut [&'a BytesRef<Vec<u8>>],
}
impl<'a, 'b> StableMSBRadixSorterTestImpl<'a, 'b> {
  fn new(final_max_length: usize, refs: &'b mut [&'a BytesRef<Vec<u8>>]) -> Self {
    StableMSBRadixSorterTestImpl {
      temp: Vec::new(),
      final_max_length,
      refs,
    }
  }
}

impl Sorter for StableMSBRadixSorterTestImpl<'_, '_> {
  fn swap(&mut self, i: usize, j: usize) -> Result<()> {
    self.refs.swap(i, j);
    Ok(())
  }
}

impl MSBRadixSorterBase for StableMSBRadixSorterTestImpl<'_, '_> {
  fn byte_at(&mut self, i: usize, k: usize) -> Result<i32> {
    assert!(k < self.final_max_length, "k is out of bounds");
    let ref_item = self.refs[i];

    if ref_item.length <= k {
      return Ok(-1);
    }

    Ok(ref_item.bytes[ref_item.offset + k] as i32)
  }
}
impl StableMSBRadixSorterBase for StableMSBRadixSorterTestImpl<'_, '_> {
  fn save(&mut self, i: usize, j: usize) {
    if self.temp.is_empty() {
      self.temp = self.refs.to_vec();
    }
    self.temp[j] = self.refs[i];
  }

  fn restore(&mut self, i: usize, j: usize) {
    if !self.temp.is_empty() {
      self.refs[i..j].copy_from_slice(&self.temp[i..j]);
    }
  }
}
