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
use crate::core::codecs::indexed_disi::{
  MAX_ARRAY_LENGTH, Owned, create_block_slice, create_jump_table,
  write_bitset_with_dense_rank_power,
};
use crate::core::codecs::lucene90::indexed_disi::{IndexedDISI, IndexedDISIImpl, Method};
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::store::directory::Directory;
use crate::core::store::random_access_input::RandomAccessInput;
use crate::core::store::{IOContext, IndexInput, IndexOutput};
use crate::test_framework::core::util::lucene_test_case::{
  at_least, new_directory, random, rarely,
};

use crate::core::util::bit_set::{BitSet, of};
use crate::core::util::bit_set_iterator::BitSetIterator;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::sparse_fixed_bit_set::SparseFixedBitSet;
use crate::test_framework::core::util::test_util::TestUtil;

use crate::core::util::TryIntoInt;
use rand::Rng;
use rand::RngExt;

#[allow(dead_code)] // for quick search
struct TestIndexedDISI;

#[test]
fn test_empty() -> Result<()> {
  let mut random = random();
  let max_doc = TestUtil::next_usize(&mut random, 1, 100_000);
  let set = SparseFixedBitSet::new(max_doc)?;
  let dir = new_directory(&mut random)?;
  let _ = do_test(set, &dir, &mut random);
  Ok(())
}

#[cfg(feature = "nightly")]
#[test]
#[ignore = "nightly"]
fn test_empty_blocks() -> Result<()> {
  const B: usize = 65536;
  let mut random = random();
  let max_doc = B * 11;
  let mut set = SparseFixedBitSet::new(max_doc)?;
  set.set(B + 5);
  set.set(B * 4 + 5);
  for i in 0..B {
    set.set(B * 6 + i);
  }
  for i in (0..B).step_by(3) {
    set.set(B * 7 + i);
  }
  for i in 0..B {
    if i != 32768 {
      set.set(B * 8 + i);
    }
  }
  {
    let dir = new_directory(&mut random)?;
    set = do_test_all_single_jump(&mut random, set, &dir)?;
  }
  set.set(0);
  {
    let dir = new_directory(&mut random)?;
    let _ = do_test_all_single_jump(&mut random, set, &dir)?;
  }
  Ok(())
}

#[test]
fn test_last_empty_blocks() -> Result<()> {
  let mut random = random();
  let dir = new_directory(&mut random)?;
  const B: usize = 65536;
  let max_doc = B * 3;
  let mut set = SparseFixedBitSet::new(max_doc)?;
  for i in 0..(B * 2) {
    set.set(i);
  }
  set = do_test_all_single_jump(&mut random, set, &dir)?;
  assert_advance_beyond_end(set, &dir)
}

fn assert_advance_beyond_end<B>(set: B, dir: &impl Directory) -> Result<()>
where
  B: BitSet,
{
  let cardinality = set.cardinality();
  let dense_rank_power = 9;
  let mut out = dir.create_output("bar", &IOContext::default_io_context()?)?;
  let mut v = BitSetIterator::new(set, cardinality as i64)?;
  let jump_count = write_bitset_with_dense_rank_power(&mut v, &mut out, dense_rank_power)? as i32;
  let length = out.get_file_pointer()?;
  drop(out);

  let mut disi2 = BitSetIterator::new(v.bits, cardinality as i64)?;
  let mut doc = disi2.doc_id();
  let mut index = 0;
  while doc < cardinality as i32 {
    doc = disi2.next_doc()?;
    index += 1;
  }

  let input = dir.open_input("bar", &IOContext::default_io_context()?)?;
  let mut disi = IndexedDISIImpl::new(
    &input,
    0,
    length,
    jump_count,
    dense_rank_power,
    cardinality as i64,
  )?;
  assert!(
    !disi.advance_exact(disi2.bits.length().try_convert()?)?,
    "There should be no set bit beyond the valid docID range"
  );
  disi.advance(doc)?;
  assert_eq!(
    index,
    disi.index_u() + 1,
    "The index when advancing beyond the last defined docID should be correct"
  );
  Ok(())
}

#[cfg(feature = "nightly")]
#[test]
#[ignore = "nightly"]
fn test_random_blocks() -> Result<()> {
  let mut random = random();
  let dir = new_directory(&mut random)?;
  let set = create_set_with_random_blocks(&mut random, 5)?;
  let _ = do_test_all_single_jump(&mut random, set, &dir)?;
  Ok(())
}

#[test]
fn test_position_not_zero() -> Result<()> {
  let mut random = random();
  let dir = new_directory(&mut random)?;
  const BLOCKS: usize = 10;
  let dense_rank_power = if rarely(&mut random) {
    -1
  } else {
    (random.random_range(0..7) + 7) as i8
  };
  let set = create_set_with_random_blocks(&mut random, BLOCKS)?;
  let cardinality = set.cardinality();
  let mut out = dir.create_output("foo", &IOContext::default_io_context()?)?;
  let jump_table_entry_count = write_bitset_with_dense_rank_power(
    &mut BitSetIterator::new(set, cardinality as i64)?,
    &mut out,
    dense_rank_power,
  )? as i32;
  let length = out.get_file_pointer()?;
  drop(out);

  let full_input = dir.open_input("foo", &IOContext::default_io_context()?)?;
  test_position_not_zero_extra(
    &mut random,
    &full_input,
    dense_rank_power,
    length,
    jump_table_entry_count,
    cardinality as i64,
    BLOCKS as i32,
  )
}
fn test_position_not_zero_extra<I, R>(
  random: &mut R,
  full_input: &I,
  dense_rank_power: i8,
  length: usize,
  jump_table_entry_count: i32,
  cardinality: i64,
  blocks: i32,
) -> Result<()>
where
  I: IndexInput,
  R: Rng + ?Sized,
{
  let mut block_data = create_block_slice(full_input, "blocks", 0, length, jump_table_entry_count)?;
  block_data.seek(random.random_range(0..block_data.length()?))?;
  let jump_table = create_jump_table(full_input, 0, length, jump_table_entry_count)?;
  let mut disi: IndexedDISI<I, Owned> = IndexedDISIImpl::from_components(
    block_data,
    jump_table,
    jump_table_entry_count,
    dense_rank_power,
    cardinality,
  )?;
  disi.advance_exact(blocks * 65536 - 1)?;
  Ok(())
}

fn create_set_with_random_blocks<R>(random: &mut R, block_count: usize) -> Result<SparseFixedBitSet>
where
  R: Rng + ?Sized,
{
  const B: usize = 65536;
  let mut set = SparseFixedBitSet::new(block_count * B)?;
  for block in 0..block_count {
    match random.random_range(0..4) {
      0 => {},
      1 => {
        for doc_id in (block * B)..((block + 1) * B) {
          set.set(doc_id);
        }
      },
      2 => {
        for doc_id in (block * B..(block + 1) * B).step_by(101) {
          set.set(doc_id);
        }
      },
      3 => {
        for doc_id in (block * B..(block + 1) * B).step_by(3) {
          set.set(doc_id);
        }
      },
      _ => unreachable!(),
    }
  }
  Ok(set)
}

fn do_test_all_single_jump<R, B>(random: &mut R, set: B, dir: &impl Directory) -> Result<B>
where
  R: Rng + ?Sized,
  B: BitSet,
{
  let cardinality = set.cardinality();
  let dense_rank_power = if rarely(random) {
    -1
  } else {
    (random.random_range(0..7) + 7) as i8
  };
  let mut out = dir.create_output("foo", &IOContext::default_io_context()?)?;
  let mut v = BitSetIterator::new(set, cardinality as i64)?;
  let jump_table_entry_count =
    { write_bitset_with_dense_rank_power(&mut v, &mut out, dense_rank_power)? as i32 };

  let length = out.get_file_pointer()?;
  drop(out);

  let input = dir.open_input("foo", &IOContext::default_io_context()?)?;
  for i in 0..v.bits.length() {
    let mut disi = IndexedDISIImpl::new(
      &input,
      0,
      length,
      jump_table_entry_count,
      dense_rank_power,
      cardinality as i64,
    )?;
    assert_eq!(v.bits.get(i)?, disi.advance_exact(i as i32)?);

    let mut disi2 = IndexedDISIImpl::new(
      &input,
      0,
      length,
      jump_table_entry_count,
      dense_rank_power,
      cardinality as i64,
    )?;
    let doc = disi2.advance(i as i32)? as usize;
    assert!(i <= doc);
    if v.bits.get(i)? {
      assert_eq!(i, doc);
    } else {
      assert_ne!(i, doc);
    }
  }
  let set = v.bits;
  Ok(set)
}
#[test]
fn test_one_doc() -> Result<()> {
  let mut random = random();
  let max_doc = TestUtil::next_usize(&mut random, 1, 100_000);
  let mut set = SparseFixedBitSet::new(max_doc)?;
  set.set(random.random_range(0..max_doc));
  let dir = new_directory(&mut random)?;
  let _ = do_test(set, &dir, &mut random)?;
  Ok(())
}

#[test]
fn test_two_docs() -> Result<()> {
  let mut random = random();
  let max_doc = TestUtil::next_usize(&mut random, 1, 100_000);
  let mut set = SparseFixedBitSet::new(max_doc)?;
  set.set(random.random_range(0..max_doc));
  set.set(random.random_range(0..max_doc));
  let dir = new_directory(&mut random)?;
  let _ = do_test(set, &dir, &mut random)?;
  Ok(())
}

#[test]
fn test_all_docs() -> Result<()> {
  let mut random = random();
  let max_doc = TestUtil::next_usize(&mut random, 1, 100_000);
  let mut set = FixedBitSet::new(max_doc);
  set.set_with_range(1, max_doc);
  let dir = new_directory(&mut random)?;
  let _ = do_test(set, &dir, &mut random)?;
  Ok(())
}

#[test]
fn test_half_full() -> Result<()> {
  let mut random = random();
  let max_doc = TestUtil::next_usize(&mut random, 1, 100_000);
  let mut set = SparseFixedBitSet::new(max_doc)?;
  let mut i = random.random_range(0..2);
  while i < max_doc {
    set.set(i);
    i += TestUtil::next_usize(&mut random, 1, 3);
  }
  let dir = new_directory(&mut random)?;
  let _ = do_test(set, &dir, &mut random)?;
  Ok(())
}

#[test]
fn test_doc_range() -> Result<()> {
  let mut random = random();
  let dir = new_directory(&mut random)?;

  for _ in 0..10 {
    let max_doc = TestUtil::next_usize(&mut random, 1, 1_000_000);
    let mut set = FixedBitSet::new(max_doc);
    let start = random.random_range(0..max_doc);
    let end = TestUtil::next_usize(&mut random, start + 1, max_doc);
    set.set_with_range(start, end);
    let _ = do_test(set, &dir, &mut random)?;
  }

  Ok(())
}

#[test]
fn test_sparse_dense_boundary() -> Result<()> {
  let mut random = random();
  let dir = new_directory(&mut random)?;
  let mut set = FixedBitSet::new(200_000);
  let start = 65536 + random.random_range(0..100);
  let dense_rank_power = if rarely(&mut random) {
    -1
  } else {
    (random.random_range(0..7) + 7) as i8
  };

  set.set_with_range(start, start + MAX_ARRAY_LENGTH as usize);
  let mut out = dir.create_output("sparse", &IOContext::default_io_context()?)?;
  let mut v = BitSetIterator::new(set, MAX_ARRAY_LENGTH as i64)?;
  let jump_table_entry_count =
    { write_bitset_with_dense_rank_power(&mut v, &mut out, dense_rank_power)? as i32 };
  let length = out.get_file_pointer()?;
  drop(out);

  let mut set = v.bits;

  {
    let input = dir.open_input("sparse", &IOContext::default_io_context()?)?;
    let mut disi = IndexedDISIImpl::new(
      &input,
      0,
      length,
      jump_table_entry_count,
      dense_rank_power,
      MAX_ARRAY_LENGTH as i64,
    )?;
    assert_eq!(start, disi.next_doc()? as usize);
    assert_eq!(Method::Sparse, disi.method);
  }

  set = do_test(set, &dir, &mut random)?;

  set.set(start + MAX_ARRAY_LENGTH as usize + random.random_range(0..100));
  let mut out = dir.create_output("bar", &IOContext::default_io_context()?)?;
  let mut v = BitSetIterator::new(set.clone(), (MAX_ARRAY_LENGTH + 1) as i64)?;
  write_bitset_with_dense_rank_power(&mut v, &mut out, dense_rank_power)?;
  let set = v.bits;
  let length = out.get_file_pointer()?;
  drop(out);

  {
    let input = dir.open_input("bar", &IOContext::default_io_context()?)?;
    let mut disi = IndexedDISIImpl::new(
      &input,
      0,
      length,
      jump_table_entry_count,
      dense_rank_power,
      (MAX_ARRAY_LENGTH + 1) as i64,
    )?;
    assert_eq!(start, disi.next_doc()? as usize);
    assert_eq!(Method::Dense, disi.method);
  }

  let _ = do_test(set, &dir, &mut random)?;
  Ok(())
}

#[test]
fn test_one_doc_missing() -> Result<()> {
  let mut random = random();
  let max_doc = TestUtil::next_usize(&mut random, 1, 1_000_000);
  let mut set = FixedBitSet::new(max_doc);
  set.set_with_range(0, max_doc);
  set.clear_with_index(random.random_range(0..max_doc));
  let dir = new_directory(&mut random)?;
  let _ = do_test(set, &dir, &mut random)?;
  Ok(())
}

#[test]
fn test_few_missing_docs() -> Result<()> {
  let mut random = random();
  let dir = new_directory(&mut random)?;
  let num_iters = at_least(&mut random, 10);

  for _ in 0..num_iters {
    let max_doc = TestUtil::next_usize(&mut random, 1, 100_000);
    let mut set = FixedBitSet::new(max_doc);
    set.set_with_range(0, max_doc);
    let num_missing = TestUtil::next_int(&mut random, 2, 1000);
    for _ in 0..num_missing {
      set.clear_with_index(random.random_range(0..max_doc));
    }
    let _ = do_test(set, &dir, &mut random)?;
  }

  Ok(())
}

#[test]
fn test_dense_multi_block() -> Result<()> {
  let mut random = random();
  let dir = new_directory(&mut random)?;
  let max_doc = 10 * 65536;
  let mut set = FixedBitSet::new(max_doc);
  for i in (0..max_doc).step_by(2) {
    set.set(i);
  }
  let _ = do_test(set, &dir, &mut random)?;
  Ok(())
}

#[test]
fn test_illegal_dense_rank_power() -> Result<()> {
  for &power in &[-1, 7, 8, 9, 10, 11, 12, 13, 14, 15] {
    create_and_open_disi(power, power)?;
  }

  for &power in &[-2, 0, 1, 6, 16] {
    assert!(matches!(
      create_and_open_disi(power, 8),
      Err(LuceneError::IllegalArgument(_))
    ));

    assert!(matches!(
      create_and_open_disi(8, power),
      Err(LuceneError::IllegalArgument(_))
    ));
  }

  Ok(())
}

fn create_and_open_disi(write_power: i8, read_power: i8) -> Result<()> {
  let mut set = FixedBitSet::new(10);
  set.set(9);
  let mut random = random();
  let dir = new_directory(&mut random)?;
  let mut out = dir.create_output("foo", &IOContext::default_io_context()?)?;
  let mut v = BitSetIterator::new(set.clone(), set.cardinality() as i64)?;
  let jump_count = write_bitset_with_dense_rank_power(&mut v, &mut out, write_power)? as i32;
  let length = out.get_file_pointer()?;
  drop(out);

  let input = dir.open_input("foo", &IOContext::default_io_context()?)?;
  let _ = IndexedDISIImpl::new(
    &input,
    0,
    length,
    jump_count,
    read_power,
    set.cardinality() as i64,
  )?;
  Ok(())
}

#[test]
fn test_one_doc_missing_fixed() -> Result<()> {
  let mut random = random();
  let max_doc = 9699;
  let dense_rank_power = if rarely(&mut random) {
    -1
  } else {
    (random.random_range(0..7) + 7) as i8
  };

  let mut set = FixedBitSet::new(max_doc);
  set.set_with_range(0, max_doc);
  set.clear_with_index(1345);
  let cardinality = set.cardinality() as i64;

  let dir = new_directory(&mut random)?;
  let mut out = dir.create_output("foo", &IOContext::default_io_context()?)?;
  let mut v = BitSetIterator::new(set, cardinality)?;
  let jump_table_entry_count =
    write_bitset_with_dense_rank_power(&mut v, &mut out, dense_rank_power)? as i32;
  let length = out.get_file_pointer()?;
  drop(out);

  let mut disi2 = BitSetIterator::new(v.bits, cardinality)?;
  let input = dir.open_input("foo", &IOContext::default_io_context()?)?;
  let mut disi = IndexedDISIImpl::new(
    &input,
    0,
    length,
    jump_table_entry_count,
    dense_rank_power,
    cardinality,
  )?;
  assert_advance_equality(&mut disi, &mut disi2, 16000)
}

#[test]
fn test_random() -> Result<()> {
  let mut random = random();
  let dir = new_directory(&mut random)?;
  let num_iters = at_least(&mut random, 3);

  for _ in 0..num_iters {
    do_test_random(&dir, &mut random)?;
  }

  Ok(())
}

fn do_test_random<R>(dir: &impl Directory, random: &mut R) -> Result<()>
where
  R: Rng + ?Sized,
{
  let end = TestUtil::next_int(random, 2, 20);
  let max_step = TestUtil::next_int(random, 1, 1 << end);
  let num_docs = TestUtil::next_int(random, 1, std::cmp::min(100_000, (i32::MAX - 1) / max_step));

  let mut docs = SparseFixedBitSet::new((num_docs * max_step + 1) as usize)?;
  let mut last_doc = -1;

  let mut doc = -1;
  for _ in 0..num_docs {
    doc += TestUtil::next_int(random, 1, max_step);
    docs.set(doc as usize);
    last_doc = doc;
  }

  let max_doc = last_doc + TestUtil::next_int(random, 1, 100);
  let cardinality = docs.approximate_cardinality();
  let mut bit_set_iterator = BitSetIterator::new(docs, cardinality as i64)?;
  let set = of(&mut bit_set_iterator, max_doc as usize)?;

  let _ = do_test(set, dir, random)?;
  Ok(())
}

fn do_test<R, B>(set: B, dir: &impl Directory, random: &mut R) -> Result<B>
where
  R: Rng + ?Sized,
  B: BitSet,
{
  let cardinality = set.cardinality() as i64;
  let dense_rank_power = if rarely(random) {
    -1
  } else {
    (random.random_range(0..7) + 7) as i8
  };

  let length;
  let jump_table_entry_count;

  let mut set = {
    let mut out = dir.create_output("foo", &IOContext::default_io_context()?)?;
    let mut v = BitSetIterator::new(set, cardinality)?;
    jump_table_entry_count =
      write_bitset_with_dense_rank_power(&mut v, &mut out, dense_rank_power)? as i32;
    length = out.get_file_pointer()?;
    v.bits
  };

  set = {
    let input = dir.open_input("foo", &IOContext::default_io_context()?)?;
    let mut disi = IndexedDISIImpl::new(
      &input,
      0,
      length,
      jump_table_entry_count,
      dense_rank_power,
      cardinality,
    )?;
    let mut disi2 = BitSetIterator::new(set, cardinality)?;
    assert_single_step_equality(&mut disi, &mut disi2)?;
    disi2.bits
  };

  for &step in &[1, 10, 100, 1000, 10000, 100000] {
    let input = dir.open_input("foo", &IOContext::default_io_context()?)?;
    let mut disi = IndexedDISIImpl::new(
      &input,
      0,
      length,
      jump_table_entry_count,
      dense_rank_power,
      cardinality,
    )?;
    let mut disi2 = BitSetIterator::new(set, cardinality)?;
    assert_advance_equality(&mut disi, &mut disi2, step)?;
    set = disi2.bits
  }

  for &step in &[10, 100, 1000, 10000, 100000] {
    let input = dir.open_input("foo", &IOContext::default_io_context()?)?;
    let mut disi = IndexedDISIImpl::new(
      &input,
      0,
      length,
      jump_table_entry_count,
      dense_rank_power,
      cardinality,
    )?;
    let disi2_length = set.length();
    let mut disi2 = BitSetIterator::new(set, cardinality)?;
    assert_advance_exact_randomized(random, &mut disi, &mut disi2, disi2_length as i32, step)?;
    set = disi2.bits
  }

  dir.delete_file("foo")?;
  Ok(set)
}

fn assert_advance_exact_randomized<I, RI, T, R>(
  random: &mut R,
  disi: &mut IndexedDISIImpl<I, RI>,
  disi2: &mut BitSetIterator<T>,
  disi2_length: i32,
  step: i32,
) -> Result<()>
where
  I: IndexInput,
  RI: RandomAccessInput,
  T: BitSet,
  R: Rng + ?Sized,
{
  let mut index = -1;
  let mut target = 0;

  while target < disi2_length {
    target += TestUtil::next_int(random, 0, step);
    let mut doc = disi2.doc_id();
    while doc < target {
      doc = disi2.next_doc()?;
      index += 1;
    }

    let exists = disi.advance_exact(target)?;
    assert_eq!(doc == target, exists);
    if exists {
      assert_eq!(index, disi.index());
    } else if random.random_bool(0.5) {
      let advanced_doc = disi.next_doc()?;
      assert_eq!(doc, advanced_doc);
      // This is a bit strange when doc == NO_MORE_DOCS as the index
      // overcounts in the disi2 while-loop
      assert_eq!(index, disi.index());
      target = doc;
    }
  }

  Ok(())
}
fn assert_single_step_equality<I, RI, T>(
  disi: &mut IndexedDISIImpl<I, RI>,
  disi2: &mut BitSetIterator<T>,
) -> Result<()>
where
  I: IndexInput,
  RI: RandomAccessInput,
  T: BitSet,
{
  let mut i = 0;
  let mut doc = disi2.next_doc()?;

  while doc != NO_MORE_DOCS {
    assert_eq!(doc, disi.next_doc()?);
    assert_eq!(i, disi.index_u());
    i += 1;
    doc = disi2.next_doc()?;
  }

  assert_eq!(NO_MORE_DOCS, disi.next_doc()?);
  Ok(())
}
fn assert_advance_equality<I, RI, T>(
  disi: &mut IndexedDISIImpl<I, RI>,
  disi2: &mut BitSetIterator<T>,
  step: i32,
) -> Result<()>
where
  I: IndexInput,
  RI: RandomAccessInput,
  T: BitSet,
{
  let mut index = -1;

  loop {
    let target = disi2.doc_id() + step;
    let mut doc;

    loop {
      doc = disi2.next_doc()?;
      index += 1;
      if doc >= target {
        break;
      }
    }

    let advanced = disi.advance(target)?;
    assert_eq!(doc, advanced);

    if doc == NO_MORE_DOCS {
      break;
    }

    assert_eq!(
      index,
      disi.index(),
      "Expected equality using step {} at docID {}",
      step,
      doc
    );
  }

  Ok(())
}
