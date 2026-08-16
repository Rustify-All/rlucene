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
// Migrated from src/core/util/merged_iterator.rs

use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::iterator::IteratorExt;
use crate::core::util::merged_iterator::MergedIterator;
use crate::test_framework::core::util::lucene_test_case::random;
use rand::Rng;
use rand::RngExt;

#[allow(dead_code)] // for quick search
struct TestMergedIterator;

#[test]
fn test_merge_empty() -> Result<()> {
  let merged: MergedIterator<EmptyIter> =
    MergedIterator::with_remove_duplicates(true, Vec::new())?;
  assert!(!merged.has_next()?);

  let empty = EmptyIter::new();
  let merged = MergedIterator::with_remove_duplicates(true, vec![empty])?;
  assert!(!merged.has_next()?);

  let mut random = random();
  let n = random.random_range(0..100);
  let mut iters = Vec::with_capacity(n);
  for _ in 0..n {
    iters.push(EmptyIter::new());
  }

  let merged = MergedIterator::with_remove_duplicates(true, iters)?;
  assert!(!merged.has_next()?);

  Ok(())
}
const VALS_TO_MERGE: usize = 15000;
const REPEATS: usize = 2;

#[test]
fn test_no_dups_remove_dups() -> Result<()> {
  let mut random = random();
  for _ in 0..REPEATS {
    test_case(&mut random, 1, 1, true)?;
  }
  Ok(())
}
#[test]
fn test_off_itr_dups_remove_dups() -> Result<()> {
  let mut random = random();
  for _ in 0..REPEATS {
    test_case(&mut random, 3, 1, true)?;
  }
  Ok(())
}

#[test]
fn test_on_itr_dups_remove_dups() -> Result<()> {
  let mut random = random();
  for _ in 0..REPEATS {
    test_case(&mut random, 1, 3, true)?;
  }
  Ok(())
}

#[test]
fn test_on_itr_random_dups_remove_dups() -> Result<()> {
  let mut random = random();
  for _ in 0..REPEATS {
    test_case(&mut random, 1, -3, true)?;
  }
  Ok(())
}

#[test]
fn test_both_dups_remove_dups() -> Result<()> {
  let mut random = random();
  for _ in 0..REPEATS {
    test_case(&mut random, 3, 3, true)?;
  }
  Ok(())
}

#[test]
fn test_both_dups_with_random_dups_remove_dups() -> Result<()> {
  let mut random = random();
  for _ in 0..REPEATS {
    test_case(&mut random, 3, -3, true)?;
  }
  Ok(())
}

#[test]
fn test_no_dups_keep_dups() -> Result<()> {
  let mut random = random();
  for _ in 0..REPEATS {
    test_case(&mut random, 1, 1, false)?;
  }
  Ok(())
}

#[test]
fn test_off_itr_dups_keep_dups() -> Result<()> {
  let mut random = random();
  for _ in 0..REPEATS {
    test_case(&mut random, 3, 1, false)?;
  }
  Ok(())
}

#[test]
fn test_on_itr_dups_keep_dups() -> Result<()> {
  let mut random = random();
  for _ in 0..REPEATS {
    test_case(&mut random, 1, 3, false)?;
  }
  Ok(())
}

#[test]
fn test_on_itr_random_dups_keep_dups() -> Result<()> {
  let mut random = random();
  for _ in 0..REPEATS {
    test_case(&mut random, 1, -3, false)?;
  }
  Ok(())
}

#[test]
fn test_both_dups_keep_dups() -> Result<()> {
  let mut random = random();
  for _ in 0..REPEATS {
    test_case(&mut random, 3, 3, false)?;
  }
  Ok(())
}

#[test]
fn test_both_dups_with_random_dups_keep_dups() -> Result<()> {
  let mut random = random();
  for _ in 0..REPEATS {
    test_case(&mut random, 3, -3, false)?;
  }
  Ok(())
}

fn test_case<R>(
  random: &mut R,
  itrs_with_val: usize,
  specified_vals_on_itr: i32,
  remove_dups: bool,
) -> Result<()>
where
  R: Rng + ?Sized,
{
  // Build a random number of lists
  let mut expected: Vec<i32> = Vec::new();
  let num_lists = itrs_with_val + random.random_range(0..(1000 - itrs_with_val));
  let mut lists: Vec<Vec<i32>> = (0..num_lists).map(|_| Vec::new()).collect();

  let start = random.random_range(0..1_000_000);
  let end = start + VALS_TO_MERGE / itrs_with_val / specified_vals_on_itr.unsigned_abs() as usize;

  for i in start..end {
    let mut max_list = lists.len();
    let mut max_vals_on_itr = 0;
    let mut sum_vals_on_itr = 0;

    for _ in 0..itrs_with_val {
      let list_idx = random.random_range(0..max_list);

      let vals_on_itr = if specified_vals_on_itr < 0 {
        1 + random.random_range(0..(-specified_vals_on_itr as usize))
      } else {
        specified_vals_on_itr as usize
      };

      max_vals_on_itr = max_vals_on_itr.max(vals_on_itr);
      sum_vals_on_itr += vals_on_itr;

      for _ in 0..vals_on_itr {
        lists[list_idx].push(i as i32);
      }

      max_list -= 1;
      lists.swap(list_idx, max_list);
    }

    let max_count = if remove_dups {
      max_vals_on_itr
    } else {
      sum_vals_on_itr
    };

    for _ in 0..max_count {
      expected.push(i as i32);
    }
  }

  // Now check that they get merged cleanly
  let itrs: Vec<ListIter<i32>> = lists.into_iter().map(ListIter::new).collect();

  let mut merged = MergedIterator::with_remove_duplicates(remove_dups, itrs)?;

  let mut expected_idx = 0;

  while expected_idx < expected.len() {
    assert!(merged.has_next()?);
    let v = merged
      .next()?
      .ok_or_else(|| LuceneError::illegal_state("expected value"))?;
    assert_eq!(expected[expected_idx], v);
    expected_idx += 1;
  }

  assert!(!merged.has_next()?);
  Ok(())
}

struct EmptyIter;

impl EmptyIter {
  fn new() -> Self {
    Self
  }
}

impl IteratorExt for EmptyIter {
  type Item = i32;

  fn next(&mut self) -> Result<Option<Self::Item>> {
    Ok(None)
  }

  fn has_next(&self) -> Result<bool> {
    Ok(false)
  }
}

struct ListIter<T> {
  data: Vec<T>,
  pos: usize,
}

impl<T> ListIter<T> {
  fn new(data: Vec<T>) -> Self {
    Self { data, pos: 0 }
  }
}

impl<T: Clone> IteratorExt for ListIter<T> {
  type Item = T;

  fn next(&mut self) -> Result<Option<Self::Item>> {
    if self.pos < self.data.len() {
      let v = self.data[self.pos].clone();
      self.pos += 1;
      Ok(Some(v))
    } else {
      Ok(None)
    }
  }

  fn has_next(&self) -> Result<bool> {
    Ok(self.pos < self.data.len())
  }
}
