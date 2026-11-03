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
use std::collections::BTreeSet;

use smallvec::SmallVec;

use crate::core::index::impact::Impact;

/// This struct accumulates the (freq, norm) pairs that may produce competitive
/// scores.
pub struct CompetitiveImpactAccumulator {
  /// We speed up accumulation for common norm values with this array that
  /// maps norm values in -128..127 to the maximum frequency observed for
  /// these norm values.
  max_freqs: [i32; 256],
  /// Stores competitive (freq, norm) pairs for norm values that fall
  /// outside of -128..127. It is always empty with the default similarity,
  /// which encodes norms as bytes.
  other_freq_norm_pairs: BTreeSet<Impact>,
}

impl Default for CompetitiveImpactAccumulator {
  fn default() -> Self {
    Self::new()
  }
}

impl CompetitiveImpactAccumulator {
  /// Creates a new instance.
  pub fn new() -> Self {
    CompetitiveImpactAccumulator {
      max_freqs: [0; 256],
      other_freq_norm_pairs: BTreeSet::new(),
    }
  }
  /// Reset to the same state it was in after creation.
  pub fn clear(&mut self) {
    self.max_freqs = [0; 256];
    if !self.other_freq_norm_pairs.is_empty() {
      self.other_freq_norm_pairs.clear();
    }
    debug_assert!(self.assert_consistent());
  }

  /// Accumulate a (freq,norm) pair.updating this structure if there is no
  /// equivalent or more competitive entry already.
  pub fn add(&mut self, freq: i32, norm: i64) {
    if (i8::MIN as i64..=i8::MAX as i64).contains(&norm) {
      let idx = (norm as i8) as u8 as usize;
      self.max_freqs[idx] = self.max_freqs[idx].max(freq);
    } else {
      let entry = Impact::new(freq, norm);
      Self::add_entry(entry, &mut self.other_freq_norm_pairs);
    }
    debug_assert!(self.assert_consistent());
  }

  /// Merge `acc` into this.
  pub fn add_all(&mut self, acc: &Self) {
    for i in 0..256 {
      self.max_freqs[i] = self.max_freqs[i].max(acc.max_freqs[i]);
    }
    for entry in &acc.other_freq_norm_pairs {
      Self::add_entry(entry.clone(), &mut self.other_freq_norm_pairs);
    }
    debug_assert!(self.assert_consistent());
  }
  /// Replace the content of this with the provided `acc`.
  pub fn copy_from(&mut self, acc: &Self) {
    self.max_freqs.copy_from_slice(&acc.max_freqs);
    self.other_freq_norm_pairs = acc.other_freq_norm_pairs.clone();
    debug_assert!(self.assert_consistent());
  }

  /// Get the set of competitive (freq,norm) pairs, ordered by freq→norm.
  pub fn get_competitive_freq_norm_pairs(&self) -> SmallVec<[Impact; 8]> {
    let mut impacts = SmallVec::new();
    let mut max_freq_for_lower_norms = 0;
    for (block_index, block) in self.max_freqs.as_chunks::<16>().0.iter().enumerate() {
      // A block below the current frontier cannot contribute a new record.
      if block.iter().copied().max().unwrap_or(0) <= max_freq_for_lower_norms {
        continue;
      }
      for (offset, &freq) in block.iter().enumerate() {
        if freq > max_freq_for_lower_norms {
          let norm_index = block_index * 16 + offset;
          impacts.push(Impact::new(freq, norm_index as i8 as i64));
          max_freq_for_lower_norms = freq;
        }
      }
    }

    if self.other_freq_norm_pairs.is_empty() {
      return impacts;
    }

    let mut freq_norm_pairs = self.other_freq_norm_pairs.clone();
    for imp in &impacts {
      Self::add_entry(imp.clone(), &mut freq_norm_pairs);
    }
    impacts.clear();
    impacts.extend(freq_norm_pairs);
    impacts
  }

  fn add_entry(new_entry: Impact, freq_norm_pairs: &mut BTreeSet<Impact>) {
    if let Some(next) = freq_norm_pairs.range(&new_entry..).next()
      && (next.norm as u64) <= (new_entry.norm as u64)
    {
      return;
    }
    freq_norm_pairs.insert(new_entry.clone());
    let mut first_to_remove = None;
    for e in freq_norm_pairs.range(..&new_entry).rev() {
      if (e.norm as u64) >= (new_entry.norm as u64) {
        first_to_remove = Some(e.clone());
      } else {
        break;
      }
    }
    if let Some(first) = first_to_remove {
      for _ in freq_norm_pairs.extract_if(first..new_entry, |_| true) {}
    }
  }

  fn assert_consistent(&self) -> bool {
    let mut prev_freq = 0;
    let mut prev_norm = 0u64;
    for impact in &self.other_freq_norm_pairs {
      debug_assert!(impact.norm < i8::MIN as i64 || impact.norm > i8::MAX as i64);
      debug_assert!(prev_freq < impact.freq);
      debug_assert!(prev_norm < impact.norm as u64);
      prev_freq = impact.freq;
      prev_norm = impact.norm as u64;
    }
    true
  }
}

impl std::fmt::Display for CompetitiveImpactAccumulator {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "[")?;
    for (index, impact) in self
      .get_competitive_freq_norm_pairs()
      .into_iter()
      .enumerate()
    {
      if index > 0 {
        write!(f, ", ")?;
      }
      write!(f, "{impact}")?;
    }
    write!(f, "]")
  }
}
