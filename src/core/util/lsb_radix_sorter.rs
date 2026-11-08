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

use crate::core::util::SliceCopyOps;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::error::lucene_error::Result;

/// An LSB radix sorter for values interpreted as `u32`.
pub struct LSBRadixSorter {
  histogram: [usize; HISTOGRAM_SIZE],
  buffer: Vec<i32>,
}
impl Default for LSBRadixSorter {
  fn default() -> Self {
    Self::new()
  }
}

impl LSBRadixSorter {
  pub fn new() -> Self {
    LSBRadixSorter {
      histogram: [0; HISTOGRAM_SIZE],
      buffer: Vec::new(),
    }
  }
  fn build_histogram(array: &[i32], len: usize, histogram: &mut [usize; 256], shift: usize) {
    for &v in &array[..len] {
      let b = (((v as u32) >> shift) & 0xFF) as usize;
      histogram[b] += 1;
    }
  }
  fn sum_histogram(histogram: &mut [usize; 256]) {
    let mut accum = 0;
    for h in histogram.iter_mut() {
      let count = *h;
      *h = accum;
      accum += count;
    }
  }
  fn reorder(
    array: &[i32],
    len: usize,
    histogram: &mut [usize; 256],
    shift: usize,
    dest: &mut [i32],
  ) {
    for &v in &array[..len] {
      let b = (((v as u32) >> shift) & 0xFF) as usize;
      let idx = histogram[b];
      dest[idx] = v;
      histogram[b] += 1;
    }
  }
  fn sort_pass(
    array: &[i32],
    len: usize,
    histogram: &mut [usize; 256],
    shift: usize,
    dest: &mut [i32],
  ) -> bool {
    histogram.fill(0);
    Self::build_histogram(array, len, histogram, shift);
    let bucket = (((array[0] as u32) >> shift) & 0xFF) as usize;
    if histogram[bucket] == len {
      return false;
    }
    Self::sum_histogram(histogram);
    Self::reorder(array, len, histogram, shift, dest);
    true
  }

  fn insertion_sort(array: &mut [i32], off: usize, len: usize) {
    let end = off + len;
    for i in off + 1..end {
      for j in (off + 1..=i).rev() {
        if array[j - 1] > array[j] {
          array.swap(j - 1, j);
        } else {
          break;
        }
      }
    }
  }
  /// Sorts `array[0..len]` in place.
  ///
  /// - `num_bits`: how many bits are required to store any of the values in
  ///   `array[0..len]`. Pass `32` if unknown.
  pub fn sort(&mut self, num_bits: usize, array: &mut [i32], len: usize) -> Result<()> {
    if len < INSERTION_SORT_THRESHOLD {
      Self::insertion_sort(array, 0, len);
      return Ok(());
    }

    ArrayUtil::grow_no_copy(&mut self.buffer, len, || 0)?;
    let mut swapped = false;
    let mut arr: &mut [i32] = array;
    let mut buf: &mut [i32] = &mut self.buffer;

    for shift in (0..num_bits).step_by(8) {
      if Self::sort_pass(arr, len, &mut self.histogram, shift, buf) {
        std::mem::swap(&mut arr, &mut buf);
        swapped = !swapped;
      }
    }
    if swapped {
      array.copy_from(&self.buffer[..len], 0);
    }
    Ok(())
  }
}

const INSERTION_SORT_THRESHOLD: usize = 30;
const HISTOGRAM_SIZE: usize = 256;
