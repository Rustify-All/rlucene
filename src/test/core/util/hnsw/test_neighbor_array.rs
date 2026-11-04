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
#[cfg(debug_assertions)]
use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::core::util::bits::Bits;
use crate::core::util::dummy::dummy_bits::DummyBits;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::hnsw::dummy::dummy_random_vector_scorer::DummyRandomVectorScorer;
use crate::core::util::hnsw::neighbor_array::NeighborArray;
use crate::core::util::hnsw::random_vector_scorer::RandomVectorScorer;

#[allow(dead_code)] // for quick search
struct TestNeighborArray;

#[test]
fn test_scores_desc_order() -> Result<()> {
  let mut neighbors = NeighborArray::<true>::new(10);

  neighbors.add_in_order(0, 1.0)?;
  neighbors.add_in_order(1, 0.8)?;

  #[cfg(debug_assertions)]
  {
    let result = catch_unwind(AssertUnwindSafe(|| {
      neighbors.add_in_order(2, 0.9).unwrap();
    }));
    assert!(result.is_err());
    if let Err(err) = result {
      if let Some(s) = err.downcast_ref::<String>() {
        assert!(
          s.contains("Nodes are added in the incorrect order!"),
          "{}",
          s
        );
      } else {
        unreachable!();
      }
    }
  }

  neighbors.insert_sorted(3, 0.9)?;
  assert_scores_equal(&[1.0, 0.9, 0.8], &neighbors);
  assert_nodes_equal(&[0, 3, 1], &neighbors);

  neighbors.insert_sorted(4, 1.0)?;
  assert_scores_equal(&[1.0, 1.0, 0.9, 0.8], &neighbors);
  assert_nodes_equal(&[0, 4, 3, 1], &neighbors);

  neighbors.insert_sorted(5, 1.1)?;
  assert_scores_equal(&[1.1, 1.0, 1.0, 0.9, 0.8], &neighbors);
  assert_nodes_equal(&[5, 0, 4, 3, 1], &neighbors);

  neighbors.insert_sorted(6, 0.8)?;
  assert_scores_equal(&[1.1, 1.0, 1.0, 0.9, 0.8, 0.8], &neighbors);
  assert_nodes_equal(&[5, 0, 4, 3, 1, 6], &neighbors);

  neighbors.insert_sorted(7, 0.8)?;
  assert_scores_equal(&[1.1, 1.0, 1.0, 0.9, 0.8, 0.8, 0.8], &neighbors);
  assert_nodes_equal(&[5, 0, 4, 3, 1, 6, 7], &neighbors);

  neighbors.remove_index(2);
  assert_scores_equal(&[1.1, 1.0, 0.9, 0.8, 0.8, 0.8], &neighbors);
  assert_nodes_equal(&[5, 0, 3, 1, 6, 7], &neighbors);

  neighbors.remove_index(0);
  assert_scores_equal(&[1.0, 0.9, 0.8, 0.8, 0.8], &neighbors);
  assert_nodes_equal(&[0, 3, 1, 6, 7], &neighbors);

  neighbors.remove_index(4);
  assert_scores_equal(&[1.0, 0.9, 0.8, 0.8], &neighbors);
  assert_nodes_equal(&[0, 3, 1, 6], &neighbors);

  neighbors.remove_last();
  assert_scores_equal(&[1.0, 0.9, 0.8], &neighbors);
  assert_nodes_equal(&[0, 3, 1], &neighbors);

  neighbors.insert_sorted(8, 0.9)?;
  assert_scores_equal(&[1.0, 0.9, 0.9, 0.8], &neighbors);
  assert_nodes_equal(&[0, 3, 8, 1], &neighbors);

  Ok(())
}
#[test]
fn test_scores_asc_order() -> Result<()> {
  let mut neighbors = NeighborArray::<false>::new(10);

  neighbors.add_in_order(0, 0.1)?;
  neighbors.add_in_order(1, 0.3)?;

  #[cfg(debug_assertions)]
  {
    let result = catch_unwind(AssertUnwindSafe(|| {
      neighbors.add_in_order(2, 0.15).unwrap();
    }));
    assert!(result.is_err());
    if let Err(err) = result {
      if let Some(s) = err.downcast_ref::<String>() {
        assert!(
          s.contains("Nodes are added in the incorrect order!"),
          "{}",
          s
        );
      } else {
        unreachable!("panic payload is not String");
      }
    }
  }

  neighbors.insert_sorted(3, 0.3)?;
  assert_scores_equal(&[0.1, 0.3, 0.3], &neighbors);
  assert_nodes_equal(&[0, 1, 3], &neighbors);

  neighbors.insert_sorted(4, 0.2)?;
  assert_scores_equal(&[0.1, 0.2, 0.3, 0.3], &neighbors);
  assert_nodes_equal(&[0, 4, 1, 3], &neighbors);

  neighbors.insert_sorted(5, 0.05)?;
  assert_scores_equal(&[0.05, 0.1, 0.2, 0.3, 0.3], &neighbors);
  assert_nodes_equal(&[5, 0, 4, 1, 3], &neighbors);

  neighbors.insert_sorted(6, 0.2)?;
  assert_scores_equal(&[0.05, 0.1, 0.2, 0.2, 0.3, 0.3], &neighbors);
  assert_nodes_equal(&[5, 0, 4, 6, 1, 3], &neighbors);

  neighbors.insert_sorted(7, 0.2)?;
  assert_scores_equal(&[0.05, 0.1, 0.2, 0.2, 0.2, 0.3, 0.3], &neighbors);
  assert_nodes_equal(&[5, 0, 4, 6, 7, 1, 3], &neighbors);

  neighbors.remove_index(2);
  assert_scores_equal(&[0.05, 0.1, 0.2, 0.2, 0.3, 0.3], &neighbors);
  assert_nodes_equal(&[5, 0, 6, 7, 1, 3], &neighbors);

  neighbors.remove_index(0);
  assert_scores_equal(&[0.1, 0.2, 0.2, 0.3, 0.3], &neighbors);
  assert_nodes_equal(&[0, 6, 7, 1, 3], &neighbors);

  neighbors.remove_index(4);
  assert_scores_equal(&[0.1, 0.2, 0.2, 0.3], &neighbors);
  assert_nodes_equal(&[0, 6, 7, 1], &neighbors);

  neighbors.remove_last();
  assert_scores_equal(&[0.1, 0.2, 0.2], &neighbors);
  assert_nodes_equal(&[0, 6, 7], &neighbors);

  neighbors.insert_sorted(8, 0.01)?;
  assert_scores_equal(&[0.01, 0.1, 0.2, 0.2], &neighbors);
  assert_nodes_equal(&[8, 0, 6, 7], &neighbors);

  Ok(())
}
#[test]
fn test_sort_asc() -> Result<()> {
  let mut neighbors = NeighborArray::<false>::new(10);

  neighbors.add_out_of_order(1, 2.0)?;
  #[cfg(debug_assertions)]
  {
    let result = catch_unwind(AssertUnwindSafe(|| {
      neighbors.add_in_order(1, 2.0).unwrap();
    }));
    assert!(result.is_err());
  }

  neighbors.add_out_of_order(2, 3.0)?;
  neighbors.add_out_of_order(5, 6.0)?;
  neighbors.add_out_of_order(3, 4.0)?;
  neighbors.add_out_of_order(7, 8.0)?;
  neighbors.add_out_of_order(6, 7.0)?;
  neighbors.add_out_of_order(4, 5.0)?;

  let unchecked = neighbors.sort(&mut DummyRandomVectorScorer)?;
  assert_eq!(unchecked.as_slice(), &[0, 1, 2, 3, 4, 5, 6]);
  assert_nodes_equal(&[1, 2, 3, 4, 5, 6, 7], &neighbors);
  assert_scores_equal(&[2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0], &neighbors);

  let mut neighbors2 = NeighborArray::<false>::new(10);
  neighbors2.add_in_order(0, 1.0)?;
  neighbors2.add_in_order(1, 2.0)?;
  neighbors2.add_in_order(4, 5.0)?;
  neighbors2.add_out_of_order(2, 3.0)?;
  neighbors2.add_out_of_order(6, 7.0)?;
  neighbors2.add_out_of_order(5, 6.0)?;
  neighbors2.add_out_of_order(3, 4.0)?;

  let unchecked = neighbors2.sort(&mut DummyRandomVectorScorer)?;
  assert_eq!(unchecked.as_slice(), &[2, 3, 5, 6]);
  assert_nodes_equal(&[0, 1, 2, 3, 4, 5, 6], &neighbors2);
  assert_scores_equal(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0], &neighbors2);

  Ok(())
}
#[test]
fn test_sort_desc() -> Result<()> {
  let mut neighbors = NeighborArray::<true>::new(10);

  neighbors.add_out_of_order(1, 7.0)?;
  #[cfg(debug_assertions)]
  {
    let result = catch_unwind(AssertUnwindSafe(|| {
      neighbors.add_in_order(1, 2.0).unwrap();
    }));
    assert!(result.is_err());
  }

  neighbors.add_out_of_order(2, 6.0)?;
  neighbors.add_out_of_order(5, 3.0)?;
  neighbors.add_out_of_order(3, 5.0)?;
  neighbors.add_out_of_order(7, 1.0)?;
  neighbors.add_out_of_order(6, 2.0)?;
  neighbors.add_out_of_order(4, 4.0)?;

  let unchecked = neighbors.sort(&mut DummyRandomVectorScorer)?;
  assert_eq!(unchecked.as_slice(), &[0, 1, 2, 3, 4, 5, 6]);
  assert_nodes_equal(&[1, 2, 3, 4, 5, 6, 7], &neighbors);
  assert_scores_equal(&[7.0, 6.0, 5.0, 4.0, 3.0, 2.0, 1.0], &neighbors);

  let mut neighbors2 = NeighborArray::<true>::new(10);
  neighbors2.add_in_order(1, 7.0)?;
  neighbors2.add_in_order(2, 6.0)?;
  neighbors2.add_in_order(5, 3.0)?;
  neighbors2.add_out_of_order(3, 5.0)?;
  neighbors2.add_out_of_order(7, 1.0)?;
  neighbors2.add_out_of_order(6, 2.0)?;
  neighbors2.add_out_of_order(4, 4.0)?;

  let unchecked = neighbors2.sort(&mut DummyRandomVectorScorer)?;
  assert_eq!(unchecked.as_slice(), &[2, 3, 5, 6]);
  assert_nodes_equal(&[1, 2, 3, 4, 5, 6, 7], &neighbors2);
  assert_scores_equal(&[7.0, 6.0, 5.0, 4.0, 3.0, 2.0, 1.0], &neighbors2);

  Ok(())
}
#[test]
fn test_add_with_scoring_function() -> Result<()> {
  let mut neighbors = NeighborArray::<true>::new(10);
  neighbors.add_out_of_order(1, f32::NAN)?;

  #[cfg(debug_assertions)]
  {
    let result = catch_unwind(AssertUnwindSafe(|| {
      neighbors.add_in_order(1, 2.0).unwrap();
    }));
    assert!(result.is_err());
  }

  neighbors.add_out_of_order(2, f32::NAN)?;
  neighbors.add_out_of_order(5, f32::NAN)?;
  neighbors.add_out_of_order(3, f32::NAN)?;
  neighbors.add_out_of_order(7, f32::NAN)?;
  neighbors.add_out_of_order(6, f32::NAN)?;
  neighbors.add_out_of_order(4, f32::NAN)?;

  let mut scorer = TestRandomVectorScorer;
  let unchecked = neighbors.sort(&mut scorer)?;
  assert_eq!(unchecked.as_slice(), &[0, 1, 2, 3, 4, 5, 6]);
  assert_nodes_equal(&[1, 2, 3, 4, 5, 6, 7], &neighbors);
  assert_scores_equal(&[7.0, 6.0, 5.0, 4.0, 3.0, 2.0, 1.0], &neighbors);

  Ok(())
}
#[test]
fn test_add_with_scoring_function_large_ord() -> Result<()> {
  let mut neighbors = NeighborArray::<true>::new(10);
  neighbors.add_out_of_order(11, f32::NAN)?;

  #[cfg(debug_assertions)]
  {
    let result = catch_unwind(AssertUnwindSafe(|| {
      neighbors.add_in_order(1, 2.0).unwrap();
    }));
    assert!(result.is_err());
  }

  neighbors.add_out_of_order(12, f32::NAN)?;
  neighbors.add_out_of_order(15, f32::NAN)?;
  neighbors.add_out_of_order(13, f32::NAN)?;
  neighbors.add_out_of_order(17, f32::NAN)?;
  neighbors.add_out_of_order(16, f32::NAN)?;
  neighbors.add_out_of_order(14, f32::NAN)?;

  let mut scorer = TestRandomVectorScorer1;
  let unchecked = neighbors.sort(&mut scorer)?;
  assert_eq!(unchecked.as_slice(), &[0, 1, 2, 3, 4, 5, 6]);
  assert_nodes_equal(&[11, 12, 13, 14, 15, 16, 17], &neighbors);
  assert_scores_equal(&[7.0, 6.0, 5.0, 4.0, 3.0, 2.0, 1.0], &neighbors);

  Ok(())
}

fn assert_scores_equal<const DESC_ORDER: bool>(
  expected: &[f32],
  neighbors: &NeighborArray<DESC_ORDER>,
) {
  for (i, &score) in expected.iter().enumerate() {
    assert!(
      (score - neighbors.scores()[i]).abs() <= 0.01,
      "Mismatch at index {}: expected {}, got {}",
      i,
      score,
      neighbors.scores()[i]
    );
  }
}

fn assert_nodes_equal<const DESC_ORDER: bool>(
  expected: &[usize],
  neighbors: &NeighborArray<DESC_ORDER>,
) {
  for (i, &node) in expected.iter().enumerate() {
    assert_eq!(
      node,
      neighbors.nodes()[i] as usize,
      "Mismatch at index {}: expected {}, got {}",
      i,
      node,
      neighbors.nodes()[i]
    );
  }
}

#[derive(Default)]
struct TestRandomVectorScorer;
impl RandomVectorScorer for TestRandomVectorScorer {
  fn score(&self, node: usize) -> Result<f32> {
    Ok((7 - node as i32 + 1) as f32)
  }

  fn max_ord(&self) -> usize {
    0
  }

  type Bits<'a, B>
    = DummyBits
  where
    B: Bits,
    Self: 'a;

  fn get_accept_ords<'a, B>(&'a self, _accept_docs: Option<B>) -> Result<Option<Self::Bits<'a, B>>>
  where
    B: Bits,
  {
    dummy_unreachable!()
  }
}

#[derive(Default)]
struct TestRandomVectorScorer1;
impl RandomVectorScorer for TestRandomVectorScorer1 {
  fn score(&self, node: usize) -> Result<f32> {
    Ok((7 - node as i32 + 11) as f32)
  }

  fn max_ord(&self) -> usize {
    0
  }

  type Bits<'a, B>
    = DummyBits
  where
    B: Bits,
    Self: 'a;

  fn get_accept_ords<'a, B>(&'a self, _accept_docs: Option<B>) -> Result<Option<Self::Bits<'a, B>>>
  where
    B: Bits,
  {
    dummy_unreachable!()
  }
}
