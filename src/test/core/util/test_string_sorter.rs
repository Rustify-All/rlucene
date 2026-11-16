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
use crate::test_framework::core::util::lucene_test_case::{at_least, random};
use rand::Rng;
use rand::RngExt;

use crate::core::index::{BytesRef, BytesRefBuilder};
use crate::core::util::bytes_ref_comparator::{BytesRefComparator, Natural};
use crate::core::util::error::lucene_error::Result;
use crate::core::util::stable_string_sorter::{StableStringSorter, StableStringSorterBase};
use crate::core::util::{MSBRadixSorterBase, NaturalOrder, Sorter, StringSorter, StringSorterBase};
use crate::test::core::util::common_method::assert_vecs_equal;
use crate::test_framework::core::util::test_util::TestUtil;

#[allow(dead_code)] // for quick search
struct TestStringSorter;

fn test(refs: Vec<BytesRef<Vec<u8>>>, len: usize) -> Result<()> {
  test_impl(&refs[..len], len, Natural::default())?;
  test_impl(&refs[..len], len, NaturalOrder)?;
  test_stable(&refs[..len], len, Natural::default())?;
  test_stable(&refs[..len], len, NaturalOrder)?;
  Ok(())
}

fn test_impl<C>(refs: &[BytesRef<Vec<u8>>], len: usize, comparator: C) -> Result<()>
where
  C: BytesRefComparator,
{
  let actual = refs.to_vec();
  let mut expected: Vec<&BytesRef<Vec<u8>>> = refs
    .iter()
    .inspect(|value| {
      debug_assert!(value.is_valid().is_ok());
    })
    .collect();
  expected.sort();
  let delegate = StringSorterTestImpl::new(actual);
  let mut string_sorter = StringSorter::new(delegate, comparator);
  string_sorter.sort(0, len)?;

  let actual = &string_sorter.get_delegate().refs;
  assert_eq!(expected.len(), actual.len());
  for (expected, actual) in expected.iter().zip(actual) {
    assert_eq!(*expected, actual);
  }
  Ok(())
}

fn test_stable<C>(refs: &[BytesRef<Vec<u8>>], len: usize, comparator: C) -> Result<()>
where
  C: BytesRefComparator,
{
  let mut expected: Vec<&BytesRef<Vec<u8>>> = refs[..len]
    .iter()
    .inspect(|value| {
      debug_assert!(value.is_valid().is_ok());
    })
    .collect();
  let mut actual = refs[..len].to_vec();
  expected.sort();

  let actual_before_sorted = &refs[..len];
  let mut ord: Vec<usize> = (0..len).collect();
  let ord_len = ord.len();
  let delegate = StableStringSorterTestImpl {
    tmp: vec![0; ord_len],
    ord: &mut ord,
    refs: &mut actual,
  };
  let string_sorter = StableStringSorter::new(delegate);
  let mut stable_string_sorter = StringSorter::new(string_sorter, comparator);
  stable_string_sorter.sort(0, len)?;
  // `actual` is not sorted, but `ord` is sorted
  assert_vecs_equal(actual_before_sorted, &actual);
  for i in 0..len {
    assert_eq!(
      expected[i], &refs[ord[i]],
      "Mismatch at index {}: expected {:?}, found {:?}",
      i, expected[i], refs[ord[i]]
    );

    if i > 0 && expected[i] == expected[i - 1] {
      assert!(
        ord[i] > ord[i - 1],
        "Not stable: ord[{}] <= ord[{}]",
        i,
        i - 1
      );
    }
  }

  Ok(())
}

#[test]
fn test_empty() -> Result<()> {
  let mut random = random();
  let len = random.random_range(0..5);
  let refs: Vec<BytesRef<Vec<u8>>> = (0..len).map(|_| BytesRef::default()).collect();
  test(refs, 0)
}

#[test]
fn test_one_value() -> Result<()> {
  let mut random = random();
  let bytes = BytesRef::from_string(&TestUtil::random_simple_string(&mut random));
  test(vec![bytes], 1)
}

#[test]
fn test_two_values() -> Result<()> {
  let mut random = random();
  let bytes1 = BytesRef::from_string(&TestUtil::random_simple_string(&mut random));
  let bytes2 = BytesRef::from_string(&TestUtil::random_simple_string(&mut random));
  test(vec![bytes1, bytes2], 2)
}

fn test_random_impl<R>(common_prefix_len: usize, max_len: usize, random: &mut R) -> Result<()>
where
  R: Rng + ?Sized,
{
  stack_or_heap_buffer!(common_prefix, u8, common_prefix_len, 30, 0);
  random.fill_bytes(common_prefix);
  let len = random.random_range(0..100000);

  let mut bytes: Vec<BytesRef<Vec<u8>>> = Vec::with_capacity(len + random.random_range(0..50));
  for _ in 0..len {
    let mut b = vec![0u8; common_prefix_len + random.random_range(0..max_len)];
    random.fill_bytes(&mut b[common_prefix_len..]);
    b[..common_prefix.len()].copy_from_slice(common_prefix);
    bytes.push(BytesRef::from_bytes(b));
  }

  test(bytes, len)
}
#[test]
fn test_random() -> Result<()> {
  let mut random = random();
  let num_iters = at_least(&mut random, 3);
  for _ in 0..num_iters {
    test_random_impl(0, 10, &mut random)?;
  }
  Ok(())
}
#[test]
fn test_random_with_lots_of_duplicates() -> Result<()> {
  let mut random = random();
  let num_iters = at_least(&mut random, 3);
  for _ in 0..num_iters {
    test_random_impl(0, 2, &mut random)?;
  }
  Ok(())
}
#[test]
fn test_random_with_shared_prefix() -> Result<()> {
  let mut random = random();
  let num_iters = at_least(&mut random, 3);
  for _ in 0..num_iters {
    let shared_prefix_len = TestUtil::next_usize(&mut random, 1, 30);
    test_random_impl(shared_prefix_len, 10, &mut random)?;
  }
  Ok(())
}
#[test]
fn test_random_with_shared_prefix_and_lots_of_duplicates() -> Result<()> {
  let mut random = random();
  let num_iters = at_least(&mut random, 3);
  for _ in 0..num_iters {
    let shared_prefix_len = TestUtil::next_usize(&mut random, 1, 30);
    test_random_impl(shared_prefix_len, 2, &mut random)?;
  }
  Ok(())
}

struct StringSorterTestImpl {
  refs: Vec<BytesRef<Vec<u8>>>,
}

impl StringSorterTestImpl {
  fn new(refs: Vec<BytesRef<Vec<u8>>>) -> Self {
    Self { refs }
  }
}
impl Sorter for StringSorterTestImpl {
  fn swap(&mut self, i: usize, j: usize) -> Result<()> {
    self.refs.swap(i, j);
    Ok(())
  }
}
impl StringSorterBase for StringSorterTestImpl {
  type Bytes = Vec<u8>;

  fn get(
    &mut self,
    _builder: &mut BytesRefBuilder<Vec<u8>>,
    result: &mut BytesRef<Vec<u8>>,
    i: usize,
  ) -> Result<()> {
    let ref_item = &self.refs[i];
    result.offset = ref_item.offset;
    result.length = ref_item.length;
    result.bytes.clone_from(&ref_item.bytes);
    Ok(())
  }
}

struct StableStringSorterTestImpl<'a> {
  tmp: Vec<usize>,
  ord: &'a mut Vec<usize>,
  refs: &'a mut [BytesRef<Vec<u8>>],
}

impl StringSorterBase for StableStringSorterTestImpl<'_> {
  type Bytes = Vec<u8>;

  fn get(
    &mut self,
    _builder: &mut BytesRefBuilder<Vec<u8>>,
    result: &mut BytesRef<Vec<u8>>,
    i: usize,
  ) -> Result<()> {
    let ref_item = &self.refs[self.ord[i]];
    result.offset = ref_item.offset;
    result.length = ref_item.length;
    result.bytes.clone_from(&ref_item.bytes);
    Ok(())
  }
}

impl StableStringSorterBase for StableStringSorterTestImpl<'_> {
  fn save(&mut self, i: usize, j: usize) {
    self.tmp[j] = self.ord[i];
  }

  fn restore(&mut self, i: usize, j: usize) {
    let copy_source = &self.tmp[i..j];
    self.ord[i..][..copy_source.len()].copy_from_slice(copy_source);
  }
}
impl Sorter for StableStringSorterTestImpl<'_> {
  fn swap(&mut self, i: usize, j: usize) -> Result<()> {
    self.ord.swap(i, j);
    Ok(())
  }
}
impl MSBRadixSorterBase for StableStringSorterTestImpl<'_> {}
