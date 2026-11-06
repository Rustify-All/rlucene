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
use crate::core::index::float_vector_values::{FloatVectorValue, FloatVectorValues};
use crate::core::index::knn_vector_values::DocIndexIterator;
use crate::core::index::vector_similarity_function::VectorSimilarityFunction;
use crate::core::search::doc_id_set_iterator::{DocIdSetIterator, NO_MORE_DOCS};
use crate::core::search::hit_queue;
use crate::core::search::score_doc::ScoreDoc;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::selector::Selector;
use crate::core::util::vector_util::{VECTOR_UTIL, VectorUtil};
use crate::core::util::{
  CoreHelper, IntroSelector, IntroSelectorBase, IntroSelectorBaseDefault, ToInt, TryIntoInt,
};
use parking_lot::Mutex;
use std::fmt::{Display, Formatter};
use std::sync::LazyLock;

/// Will scalar quantize float vectors into `int8` byte values. This is a lossy transformation.
/// Scalar quantization works by first calculating the quantiles of the float vector values. The
/// quantiles are calculated using the configured confidence interval. The `[min_quantile,
/// max_quantile]` are then used to scale the values into the range `[0, 127]` and bucketed into the
/// nearest byte values.
///
/// ## How Scalar Quantization Works
///
/// The basic mathematical equations behind this are fairly straight forward and based on min/max
/// normalization. Given a float vector `v` and a confidence interval `q` we can calculate the
/// quantiles of the vector values `[min_quantile, max_quantile]`.
///
/// ```text
/// byte = (float - min_quantile) * 127/(max_quantile - min_quantile)
/// float = (max_quantile - min_quantile)/127 * byte + min_quantile
/// ```
///
/// This means that when multiplying two `f32` values (for example, in a dot product), we can
/// following:
///
/// ```text
/// float1 * float2 ~= (byte1 * (max_quantile - min_quantile)/127 + min_quantile) * (byte2 * (max_quantile - min_quantile)/127 + min_quantile)
/// float1 * float2 ~= (byte1 * byte2 * (max_quantile - min_quantile)^2)/(127^2) + (byte1 * min_quantile * (max_quantile - min_quantile)/127) + (byte2 * min_quantile * (max_quantile - min_quantile)/127) + min_quantile^2
/// let alpha = (max_quantile - min_quantile)/127
/// float1 * float2 ~= (byte1 * byte2 * alpha^2) + (byte1 * min_quantile * alpha) + (byte2 * min_quantile * alpha) + min_quantile^2
/// ```
///
/// The expansion for square distance is much simpler:
///
/// ```text
/// square_distance = (float1 - float2)^2
/// (float1 - float2)^2 ~= (byte1 * alpha + min_quantile - byte2 * alpha - min_quantile)^2
/// = (alpha*byte1 + min_quantile)^2 + (alpha*byte2 + min_quantile)^2 - 2*(alpha*byte1 + min_quantile)(alpha*byte2 + min_quantile)
/// this can be simplified to:
/// = alpha^2 (byte1 - byte2)^2
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct ScalarQuantizer {
  alpha: f32,
  scale: f32,
  bits: u8,
  min_quantile: f32,
  max_quantile: f32,
}

pub const SCALAR_QUANTIZATION_SAMPLE_SIZE: usize = 25_000;
// 20*dimension provides protection from extreme confidence intervals
// and also prevents humongous allocations
pub(crate) const SCRATCH_SIZE: usize = 20;
const NEAREST_NEIGHBOR_COUNT: usize = 10;

impl ScalarQuantizer {
  /// - `min_quantile`: the lower quantile of the distribution
  /// - `max_quantile`: the upper quantile of the distribution
  /// - `bits`: the number of bits to use for quantization
  pub fn new(min_quantile: f32, max_quantile: f32, bits: u8) -> Result<Self> {
    if min_quantile.is_nan()
      || min_quantile.is_infinite()
      || max_quantile.is_nan()
      || max_quantile.is_infinite()
    {
      return Err(LuceneError::illegal_state(
        "Scalar quantizer does not support infinite or NaN values",
      ));
    }
    debug_assert!(max_quantile >= min_quantile);
    debug_assert!(bits > 0 && bits <= 8);
    let divisor = ((1u32 << bits) - 1) as f32;
    Ok(Self {
      alpha: (max_quantile - min_quantile) / divisor,
      scale: divisor / (max_quantile - min_quantile),
      bits,
      min_quantile,
      max_quantile,
    })
  }

  /// Quantize a float vector into a byte vector
  ///
  /// - `src`: the source vector
  /// - `dest`: the destination vector
  /// - `similarity_function`: the similarity function used to calculate the quantile
  ///
  /// Returns the corrective offset that needs to be applied to the score.
  pub fn quantize(
    &self,
    src: &[f32],
    dest: &mut [u8],
    similarity_function: VectorSimilarityFunction,
  ) -> f32 {
    debug_assert_eq!(src.len(), dest.len());
    debug_assert!(
      similarity_function != VectorSimilarityFunction::Cosine || VECTOR_UTIL.is_unit_vector(src)
    );
    if similarity_function == VectorSimilarityFunction::Euclidean
      && src.len() == dest.len()
      && src.len() >= 16
      && (1..=8).contains(&self.bits)
      && self.scale > 0.0
      && self.scale.is_finite()
    {
      self.quantize_euclidean(src, dest);
      return 0.0;
    }

    let mut correction = 0.0;
    for (i, &value) in src.iter().enumerate() {
      correction += self.quantize_float(value, Some(dest), i);
    }
    if similarity_function == VectorSimilarityFunction::Euclidean {
      return 0.0;
    }
    correction
  }

  #[inline(never)]
  fn quantize_euclidean(&self, src: &[f32], dest: &mut [u8]) {
    use wide::{bytemuck::cast, f32x4, i8x16, i16x8, i32x4, u8x16};

    let min = f32x4::splat(self.min_quantile);
    let max = f32x4::splat(self.max_quantile);
    let scale = f32x4::splat(self.scale);
    let half = f32x4::HALF;
    let quantize = |values: [f32; 4]| {
      let values = f32x4::new(values);
      let clipped = min.max(max.min(values));
      let scaled = (clipped - min) * scale;
      // Below one half, adding one half can round up to one in f32.
      let round_nonzero: i32x4 = cast(scaled.simd_ge(half) & values.simd_eq(values));
      (scaled + half).fast_trunc_int() & round_nonzero
    };
    let (src, tail) = src.as_chunks::<16>();
    let (dest, dest_tail) = dest.as_chunks_mut::<16>();
    let mask = const { i16x8::splat(0xff) };
    for (values, output) in src.iter().zip(dest) {
      let (values, _) = values.as_chunks::<4>();
      let first = quantize(values[0]);
      let second = quantize(values[1]);
      let third = quantize(values[2]);
      let fourth = quantize(values[3]);
      let low = i16x8::from_i32x8_truncate(cast([first, second]));
      let high = i16x8::from_i32x8_truncate(cast([third, fourth]));
      // Mask before unsigned narrowing to preserve Java's low-byte cast.
      *output = cast(u8x16::narrow_i16x8(low & mask, high & mask));
    }
    let (src, tail) = tail.as_chunks::<4>();
    let (dest, dest_tail) = dest_tail.as_chunks_mut::<4>();
    for (&values, output) in src.iter().zip(dest) {
      let rounded = quantize(values);
      // Truncate to the low byte, matching Java's byte cast.
      let shorts = i16x8::from_i32x8_truncate(cast([rounded, i32x4::splat(0)]));
      let bytes = i8x16::from_i16x16_truncate(cast([shorts, i16x8::splat(0)]));
      let bytes: [u8; 16] = cast(bytes);
      output.copy_from_slice(&bytes[..4]);
    }
    for (i, &value) in tail.iter().enumerate() {
      self.quantize_float(value, Some(dest_tail), i);
    }
  }

  fn quantize_float(&self, v: f32, dest: Option<&mut [u8]>, dest_index: usize) -> f32 {
    debug_assert!(dest.as_ref().is_none_or(|dest| dest_index < dest.len()));
    // Make sure the value is within the quantile range, cutting off the tails
    // see first parenthesis in equation: byte = (float - minQuantile) * 127/(maxQuantile -
    // minQuantile)
    let dx = v - self.min_quantile;
    // Bounds are finite, and integer quantization discards the sign of a zero.
    // Preserve input NaN for Java rounding and correction behavior.
    let dxc = if v.is_nan() {
      v
    } else {
      self.min_quantile.max(self.max_quantile.min(v))
    } - self.min_quantile;
    // Scale the value to the range [0, 127], this is our quantized value
    // scale = 127/(maxQuantile - minQuantile)
    let dxs = self.scale * dxc;
    // Clipping makes dxs nonnegative (or NaN); round agrees with Java Math.round
    // here. Keep its integer result before byte narrowing and dequantization.
    let rounded = dxs.round() as i32;
    // We multiply by `alpha` here to get the quantized value back into the original range
    // to aid in calculating the corrective offset
    let dxq = rounded as f32 * self.alpha;
    if let Some(dest) = dest {
      dest[dest_index] = rounded as u8;
    }
    // Calculate the corrective offset that needs to be applied to the score
    // in addition to the `byte * minQuantile * alpha` term in the equation
    // we add the `(dx - dxq) * dxq` term to account for the fact that the quantized value
    // will be rounded to the nearest whole number and lose some accuracy
    // Additionally, we account for the global correction of `minQuantile^2` in the equation
    self.min_quantile * (v - self.min_quantile / 2.0) + (dx - dxq) * dxq
  }

  /// Recalculate the old score corrective value given new current quantiles
  ///
  /// - `quantized_vector`: the old vector
  /// - `old_quantizer`: the old quantizer
  /// - `similarity_function`: the similarity function used to calculate the quantile
  ///
  /// Returns the new offset.
  pub fn recalculate_corrective_offset(
    &self,
    quantized_vector: &[u8],
    old_quantizer: &ScalarQuantizer,
    similarity_function: VectorSimilarityFunction,
  ) -> f32 {
    if similarity_function == VectorSimilarityFunction::Euclidean {
      return 0.0;
    }
    let mut corrective_offset = 0.0;
    for &i in quantized_vector {
      // dequantize the old value in order to recalculate the corrective offset
      let v = (old_quantizer.alpha * i as i8 as f32) + old_quantizer.min_quantile;
      corrective_offset += self.quantize_float(v, None, 0);
    }
    corrective_offset
  }

  /// Dequantize a byte vector into a float vector
  ///
  /// - `src`: the source vector
  /// - `dest`: the destination vector
  #[cfg(test)]
  pub(crate) fn de_quantize(&self, src: &[u8], dest: &mut [f32]) {
    debug_assert_eq!(src.len(), dest.len());
    for i in 0..src.len() {
      dest[i] = (self.alpha * src[i] as i8 as f32) + self.min_quantile;
    }
  }

  pub fn get_lower_quantile(&self) -> f32 {
    self.min_quantile
  }

  pub fn get_upper_quantile(&self) -> f32 {
    self.max_quantile
  }

  pub fn get_constant_multiplier(&self) -> f32 {
    self.alpha * self.alpha
  }

  pub fn get_bits(&self) -> u8 {
    self.bits
  }
}

impl Display for ScalarQuantizer {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(
      f,
      "ScalarQuantizer{{minQuantile={}, maxQuantile={}, bits={}}}",
      self.min_quantile, self.max_quantile, self.bits
    )
  }
}

static RANDOM: LazyLock<Mutex<JavaRandom>> = LazyLock::new(|| Mutex::new(JavaRandom::new(42)));

fn reservoir_sample_indices(num_float_vecs: usize, sample_size: usize) -> Vec<usize> {
  let mut vectors_to_take = (0..sample_size).collect::<Vec<_>>();
  let mut random = RANDOM.lock();
  for i in sample_size..num_float_vecs {
    let j = random.next_int((i + 1) as i32) as usize;
    if j < sample_size {
      vectors_to_take[j] = i;
    }
  }
  drop(random);
  vectors_to_take.sort_unstable();
  vectors_to_take
}

impl ScalarQuantizer {
  /// This will read the float vector values and calculate the quantiles. If the number of float
  /// vectors is less than [`SCALAR_QUANTIZATION_SAMPLE_SIZE`] then all the values will be read
  /// and the quantiles calculated. If the number of float vectors is greater than
  /// [`SCALAR_QUANTIZATION_SAMPLE_SIZE`] then a random sample of
  /// [`SCALAR_QUANTIZATION_SAMPLE_SIZE`] will be read and the quantiles calculated.
  ///
  /// - `float_vector_values`: the float vector values from which to calculate the quantiles
  /// - `confidence_interval`: the confidence interval used to calculate the quantiles
  /// - `total_vector_count`: the total number of live float vectors in the index. This is vital
  ///   for accounting for deleted documents when calculating the quantiles.
  /// - `bits`: the number of bits to use for quantization
  ///
  /// Returns a new [`ScalarQuantizer`] instance.
  pub fn from_vectors<F>(
    float_vector_values: &F,
    confidence_interval: f32,
    total_vector_count: usize,
    bits: u8,
  ) -> Result<Self>
  where
    F: FloatVectorValues,
  {
    Self::from_vectors_with_sample_size(
      float_vector_values,
      confidence_interval,
      total_vector_count,
      bits,
      SCALAR_QUANTIZATION_SAMPLE_SIZE,
    )
  }

  pub(crate) fn from_vectors_with_sample_size<F>(
    float_vector_values: &F,
    confidence_interval: f32,
    total_vector_count: usize,
    bits: u8,
    quantization_sample_size: usize,
  ) -> Result<Self>
  where
    F: FloatVectorValues,
  {
    debug_assert!((0.9..=1.0).contains(&confidence_interval));
    debug_assert!(quantization_sample_size > SCRATCH_SIZE);
    if total_vector_count == 0 {
      return Self::new(0.0, 0.0, bits);
    }
    let mut iterator = float_vector_values.iterator()?;
    if confidence_interval == 1.0 {
      let mut min = f32::INFINITY;
      let mut max = f32::NEG_INFINITY;
      while iterator.next_doc()? != NO_MORE_DOCS {
        let ord: usize = iterator.index()?.try_convert()?;
        let vector_value = float_vector_values.vector_value(ord)?;
        for &value in vector_value.as_ref() {
          min = CoreHelper::min_f32(min, value);
          max = CoreHelper::max_f32(max, value);
        }
      }
      return Self::new(min, max, bits);
    }
    let mut quantile_gathering_scratch =
      vec![0.0; float_vector_values.dimension() * SCRATCH_SIZE.min(total_vector_count)];
    let mut count = 0usize;
    let mut upper_sum = [0.0];
    let mut lower_sum = [0.0];
    let confidence_intervals = [confidence_interval];
    if total_vector_count <= quantization_sample_size {
      let scratch_size = SCRATCH_SIZE.min(total_vector_count);
      let mut i = 0usize;
      while iterator.next_doc()? != NO_MORE_DOCS {
        let ord: usize = iterator.index()?.try_convert()?;
        let vector_value = float_vector_values.vector_value(ord)?;
        let vector_value = vector_value.as_ref();
        let start = i * vector_value.len();
        quantile_gathering_scratch[start..start + vector_value.len()].copy_from_slice(vector_value);
        i += 1;
        if i == scratch_size {
          extract_quantiles(
            &confidence_intervals,
            &mut quantile_gathering_scratch,
            &mut upper_sum,
            &mut lower_sum,
          )?;
          i = 0;
          count += 1;
        }
      }
      // Note, we purposefully don't use the rest of the scratch state if we have fewer than
      // `SCRATCH_SIZE` vectors, mainly because if we are sampling so few vectors then we don't
      // want to be adversely affected by the extreme confidence intervals over small sample sizes
      return Self::new(
        (lower_sum[0] as f32) / count as f32,
        (upper_sum[0] as f32) / count as f32,
        bits,
      );
    }
    let vectors_to_take = reservoir_sample_indices(total_vector_count, quantization_sample_size);
    let mut index = 0usize;
    let mut idx = 0usize;
    for i in vectors_to_take {
      while index <= i {
        // We cannot use `advance(docId)` as MergedVectorValues does not support it
        iterator.next_doc()?;
        index += 1;
      }
      debug_assert!(iterator.doc_id() != NO_MORE_DOCS);
      let ord: usize = iterator.index()?.try_convert()?;
      let vector_value = float_vector_values.vector_value(ord)?;
      let vector_value = vector_value.as_ref();
      let start = idx * vector_value.len();
      quantile_gathering_scratch[start..start + vector_value.len()].copy_from_slice(vector_value);
      idx += 1;
      if idx == SCRATCH_SIZE {
        extract_quantiles(
          &confidence_intervals,
          &mut quantile_gathering_scratch,
          &mut upper_sum,
          &mut lower_sum,
        )?;
        count += 1;
        idx = 0;
      }
    }
    Self::new(
      (lower_sum[0] as f32) / count as f32,
      (upper_sum[0] as f32) / count as f32,
      bits,
    )
  }

  pub fn from_vectors_auto_interval<F>(
    float_vector_values: &F,
    function: VectorSimilarityFunction,
    total_vector_count: usize,
    bits: u8,
  ) -> Result<Self>
  where
    F: FloatVectorValues,
  {
    debug_assert!(function != VectorSimilarityFunction::Cosine);
    if total_vector_count == 0 {
      return Self::new(0.0, 0.0, bits);
    }

    let sample_size = total_vector_count.min(1000);
    let mut quantile_gathering_scratch =
      vec![0.0; float_vector_values.dimension() * SCRATCH_SIZE.min(total_vector_count)];
    let mut count = 0usize;
    let mut upper_sum = [0.0, 0.0];
    let mut lower_sum = [0.0, 0.0];
    let mut sampled_docs = Vec::with_capacity(sample_size);
    let confidence_intervals = [
      1.0
        - CoreHelper::min_f32(32.0, float_vector_values.dimension() as f32 / 10.0)
          / (float_vector_values.dimension() as f32 + 1.0),
      1.0 - 1.0 / (float_vector_values.dimension() as f32 + 1.0),
    ];
    let mut iterator = float_vector_values.iterator()?;
    if total_vector_count <= sample_size {
      let scratch_size = SCRATCH_SIZE.min(total_vector_count);
      let mut i = 0usize;
      while iterator.next_doc()? != NO_MORE_DOCS {
        let ord: usize = iterator.index()?.try_convert()?;
        let vector_value = float_vector_values.vector_value(ord)?;
        gather_sample(
          vector_value.into(),
          &mut quantile_gathering_scratch,
          &mut sampled_docs,
          i,
        )?;
        i += 1;
        if i == scratch_size {
          extract_quantiles(
            &confidence_intervals,
            &mut quantile_gathering_scratch,
            &mut upper_sum,
            &mut lower_sum,
          )?;
          i = 0;
          count += 1;
        }
      }
    } else {
      // Reservoir sample the vector ordinals we want to read
      let vectors_to_take = reservoir_sample_indices(total_vector_count, 1000);
      let mut index = 0usize;
      let mut idx = 0usize;
      for i in vectors_to_take {
        while index <= i {
          // We cannot use `advance(docId)` as MergedVectorValues does not support it
          iterator.next_doc()?;
          index += 1;
        }
        debug_assert!(iterator.doc_id() != NO_MORE_DOCS);
        let ord: usize = iterator.index()?.try_convert()?;
        let vector_value = float_vector_values.vector_value(ord)?;
        gather_sample(
          vector_value.into(),
          &mut quantile_gathering_scratch,
          &mut sampled_docs,
          idx,
        )?;
        idx += 1;
        if idx == SCRATCH_SIZE {
          extract_quantiles(
            &confidence_intervals,
            &mut quantile_gathering_scratch,
            &mut upper_sum,
            &mut lower_sum,
          )?;
          count += 1;
          idx = 0;
        }
      }
    }

    // Here we gather the upper and lower bounds for the quantile grid search
    let al = (lower_sum[1] as f32) / count as f32;
    let bu = (upper_sum[1] as f32) / count as f32;
    let au = (lower_sum[0] as f32) / count as f32;
    let bl = (upper_sum[0] as f32) / count as f32;
    if al.is_nan()
      || al.is_infinite()
      || au.is_nan()
      || au.is_infinite()
      || bl.is_nan()
      || bl.is_infinite()
      || bu.is_nan()
      || bu.is_infinite()
    {
      return Err(LuceneError::illegal_state(
        "Quantile calculation resulted in NaN or infinite values",
      ));
    }
    let mut lower_candidates = [0.0; 16];
    let mut upper_candidates = [0.0; 16];
    let mut idx = 0usize;
    let mut i = 0.0;
    while i < 32.0 {
      lower_candidates[idx] = al + i * (au - al) / 32.0;
      upper_candidates[idx] = bl + i * (bu - bl) / 32.0;
      idx += 1;
      i += 2.0;
    }
    // Now we need to find the best candidate pair by correlating the true quantized nearest
    // neighbor scores
    // with the float vector scores
    let nearest_neighbors = find_nearest_neighbors(&sampled_docs, function)?;
    let best_pair = candidate_grid_search(
      &nearest_neighbors,
      &sampled_docs,
      &lower_candidates,
      &upper_candidates,
      function,
      bits,
    )?;
    Self::new(best_pair[0], best_pair[1], bits)
  }
}

fn extract_quantiles(
  confidence_intervals: &[f32],
  quantile_gathering_scratch: &mut [f32],
  upper_sum: &mut [f64],
  lower_sum: &mut [f64],
) -> Result<()> {
  debug_assert_eq!(confidence_intervals.len(), upper_sum.len());
  debug_assert_eq!(confidence_intervals.len(), lower_sum.len());
  for i in 0..confidence_intervals.len() {
    let upper_and_lower =
      get_upper_and_lower_quantile(quantile_gathering_scratch, confidence_intervals[i])?;
    upper_sum[i] += upper_and_lower[1] as f64;
    lower_sum[i] += upper_and_lower[0] as f64;
  }
  Ok(())
}

fn gather_sample(
  vector_value: FloatVectorValue<'_>,
  quantile_gathering_scratch: &mut [f32],
  sampled_docs: &mut Vec<Vec<f32>>,
  i: usize,
) -> Result<()> {
  let vector_value = vector_value.into_owned();
  let len = vector_value.len();
  sampled_docs.push(vector_value);
  let start = i * len;
  quantile_gathering_scratch[start..start + len]
    .copy_from_slice(&sampled_docs[sampled_docs.len() - 1]);
  Ok(())
}

fn candidate_grid_search(
  nearest_neighbors: &[ScoreDocsAndScoreVariance],
  vectors: &[Vec<f32>],
  lower_candidates: &[f32],
  upper_candidates: &[f32],
  function: VectorSimilarityFunction,
  bits: u8,
) -> Result<[f32; 2]> {
  let mut max_corr = f64::NEG_INFINITY;
  let mut best_lower = 0.0;
  let mut best_upper = 0.0;
  let mut score_error_correlator =
    ScoreErrorCorrelator::new(function, nearest_neighbors, vectors, bits);
  // first do a coarse grained search to find the initial best candidate pair
  let mut best_quadrant_lower = 0usize;
  let mut best_quadrant_upper = 0usize;
  for i in (0..lower_candidates.len()).step_by(4) {
    let lower = lower_candidates[i];
    if lower.is_nan() || lower.is_infinite() {
      debug_assert!(false, "Lower candidate is NaN or infinite");
      continue;
    }
    for j in (0..upper_candidates.len()).step_by(4) {
      let upper = upper_candidates[j];
      if upper.is_nan() || upper.is_infinite() {
        debug_assert!(false, "Upper candidate is NaN or infinite");
        continue;
      }
      if upper <= lower {
        continue;
      }
      let mean = score_error_correlator.score_error_correlation(lower, upper)?;
      if mean > max_corr {
        max_corr = mean;
        best_lower = lower;
        best_upper = upper;
        best_quadrant_lower = i;
        best_quadrant_upper = j;
      }
    }
  }
  // Now search within the best quadrant
  for lower in lower_candidates
    .iter()
    .take(best_quadrant_lower + 4)
    .skip(best_quadrant_lower + 1)
    .copied()
  {
    for upper in upper_candidates
      .iter()
      .take(best_quadrant_upper + 4)
      .skip(best_quadrant_upper + 1)
      .copied()
    {
      if lower.is_nan() || lower.is_infinite() || upper.is_nan() || upper.is_infinite() {
        debug_assert!(false, "Lower or upper candidate is NaN or infinite");
        continue;
      }
      if upper <= lower {
        continue;
      }
      let mean = score_error_correlator.score_error_correlation(lower, upper)?;
      if mean > max_corr {
        max_corr = mean;
        best_lower = lower;
        best_upper = upper;
      }
    }
  }
  Ok([best_lower, best_upper])
}

/// - `vectors`: The vectors to find the nearest neighbors for each other
/// - `similarity_function`: The similarity function to use
///
/// Returns up to [`NEAREST_NEIGHBOR_COUNT`] nearest neighbors for each vector.
fn find_nearest_neighbors(
  vectors: &[Vec<f32>],
  similarity_function: VectorSimilarityFunction,
) -> Result<Vec<ScoreDocsAndScoreVariance>> {
  let mut queues = Vec::with_capacity(vectors.len());
  queues.push(hit_queue::new(NEAREST_NEIGHBOR_COUNT, false)?);
  for i in 0..vectors.len() {
    let vector = &vectors[i];
    for j in i + 1..vectors.len() {
      let other_vector = &vectors[j];
      let score = similarity_function.compare_f32(vector, other_vector)?;
      // initialize the rest of the queues
      if queues.len() <= j {
        queues.push(hit_queue::new(NEAREST_NEIGHBOR_COUNT, false)?);
      }
      queues[i].insert_with_overflow(ScoreDoc::new(j as i32, score))?;
      queues[j].insert_with_overflow(ScoreDoc::new(i as i32, score))?;
    }
  }
  // Extract the nearest neighbors from each queue.
  let mut result = Vec::with_capacity(vectors.len());
  let mut mean_and_var = OnlineMeanAndVar::default();
  for mut queue in queues {
    let score_docs_len = queue.size();
    let mut score_docs = std::array::from_fn(|_| ScoreDoc::default());
    for j in (0..queue.size()).rev() {
      let score_doc = queue
        .pop()?
        .ok_or_else(|| LuceneError::illegal_state("score doc should exist"))?;
      mean_and_var.add(score_doc.score as f64);
      score_docs[j] = score_doc;
    }
    result.push(ScoreDocsAndScoreVariance::new(
      score_docs,
      score_docs_len,
      mean_and_var.var(),
    ));
    mean_and_var.reset();
  }
  Ok(result)
}

/// Takes an array of floats, sorted or not, and returns a minimum and maximum value. These values
/// are such that they reside on the `(1 - confidence_interval)/2` and `confidence_interval/2`
/// percentiles. Example: providing floats `[0..100]` and asking for `90` quantiles will return
/// `5` and `95`.
///
/// - `arr`: array of floats
/// - `confidence_interval`: the configured confidence interval
///
/// Returns lower and upper quantile values.
pub(crate) fn get_upper_and_lower_quantile(
  arr: &mut [f32],
  confidence_interval: f32,
) -> Result<[f32; 2]> {
  debug_assert!(!arr.is_empty());
  // If we have 1 or 2 values, we can't calculate the quantiles, simply return the min and max
  if arr.len() <= 2 {
    arr.sort_by(|&left, &right| CoreHelper::compare_f32(left, right));
    return Ok([arr[0], arr[arr.len() - 1]]);
  }
  let selector_index = (arr.len() as f32 * (1.0 - confidence_interval) / 2.0 + 0.5) as usize;
  if selector_index > 0 {
    let len = arr.len();
    let mut selector = IntroSelector::new(FloatSelector::new(arr));
    Selector::select(&mut selector, 0, len, len - selector_index)?;
    Selector::select(&mut selector, 0, len - selector_index, selector_index)?;
  }
  let end = arr.len() - selector_index;
  if end.saturating_sub(selector_index) >= 8
    && let Some(values) = arr.get(selector_index..end)
  {
    use wide::{bytemuck::cast, i32x8};

    let (groups, tail) = values.as_chunks::<8>();
    let magnitude = i32x8::splat(i32::MAX);
    let mut minimum = i32x8::splat(i32::MAX);
    let mut maximum = i32x8::splat(i32::MIN);
    for &group in groups {
      let bits: i32x8 = cast(group);
      // Flip the magnitude bits of negative values to preserve order, including -0.0.
      let ordered = bits ^ ((bits >> 31) & magnitude);
      minimum = minimum.min(ordered);
      maximum = maximum.max(ordered);
    }
    let min_bits = minimum.reduce_min();
    let max_bits = maximum.reduce_max();
    let mut min = f32::from_bits((min_bits ^ ((min_bits >> 31) & i32::MAX)) as u32);
    let mut max = f32::from_bits((max_bits ^ ((max_bits >> 31) & i32::MAX)) as u32);
    // NaNs sort outside the infinities. Keep the original payload selection on that path.
    if !min.is_nan() && !max.is_nan() {
      for &value in tail {
        min = CoreHelper::min_f32(min, value);
        max = CoreHelper::max_f32(max, value);
      }
      return Ok([min, max]);
    }
  }
  let mut min = f32::INFINITY;
  let mut max = f32::NEG_INFINITY;
  for &value in arr.iter().take(end).skip(selector_index) {
    min = CoreHelper::min_f32(min, value);
    max = CoreHelper::max_f32(max, value);
  }
  Ok([min, max])
}

struct FloatSelector<'a> {
  pivot: f32,
  arr: &'a mut [f32],
}

impl<'a> FloatSelector<'a> {
  fn new(arr: &'a mut [f32]) -> Self {
    Self {
      pivot: f32::NAN,
      arr,
    }
  }
}

impl Selector for FloatSelector<'_> {
  fn swap(&mut self, i: usize, j: usize) -> Result<()> {
    self.arr.swap(i, j);
    Ok(())
  }
}

impl IntroSelectorBaseDefault for FloatSelector<'_> {
  fn set_pivot(&mut self, i: usize) -> Result<()> {
    self.pivot = self.arr[i];
    Ok(())
  }

  fn compare_pivot(&mut self, j: usize) -> Result<i32> {
    Ok(CoreHelper::compare_f32(self.pivot, self.arr[j]).to_int())
  }
}

impl IntroSelectorBase for FloatSelector<'_> {}

#[derive(Clone)]
struct ScoreDocsAndScoreVariance {
  score_docs: [ScoreDoc; NEAREST_NEIGHBOR_COUNT],
  score_docs_len: usize,
  score_variance: f32,
}

impl ScoreDocsAndScoreVariance {
  fn new(
    score_docs: [ScoreDoc; NEAREST_NEIGHBOR_COUNT],
    score_docs_len: usize,
    score_variance: f32,
  ) -> Self {
    Self {
      score_docs,
      score_docs_len,
      score_variance,
    }
  }
}

#[derive(Default)]
struct OnlineMeanAndVar {
  mean: f64,
  var: f64,
  n: usize,
}

impl OnlineMeanAndVar {
  fn reset(&mut self) {
    self.mean = 0.0;
    self.var = 0.0;
    self.n = 0;
  }

  fn add(&mut self, x: f64) {
    self.n += 1;
    let delta = x - self.mean;
    self.mean += delta / self.n as f64;
    self.var += delta * (x - self.mean);
  }

  fn var(&self) -> f32 {
    (self.var / (self.n as f64 - 1.0)) as f32
  }
}

/// This struct is used to correlate the scores of the nearest neighbors with the errors in the
/// scores. This is used to find the best quantile pair for the scalar quantizer.
struct ScoreErrorCorrelator<'a> {
  corr: OnlineMeanAndVar,
  errors: OnlineMeanAndVar,
  function: VectorSimilarityFunction,
  nearest_neighbors: &'a [ScoreDocsAndScoreVariance],
  vectors: &'a [Vec<f32>],
  // Two equal-length quantized views with a shared scratch lifetime.
  quantized: Vec<u8>,
  bits: u8,
}

impl<'a> ScoreErrorCorrelator<'a> {
  fn new(
    function: VectorSimilarityFunction,
    nearest_neighbors: &'a [ScoreDocsAndScoreVariance],
    vectors: &'a [Vec<f32>],
    bits: u8,
  ) -> Self {
    Self {
      corr: OnlineMeanAndVar::default(),
      errors: OnlineMeanAndVar::default(),
      function,
      nearest_neighbors,
      quantized: vec![0; vectors[0].len() * 2],
      bits,
      vectors,
    }
  }

  fn score_error_correlation(&mut self, lower_quantile: f32, upper_quantile: f32) -> Result<f64> {
    self.corr.reset();
    let quantizer = ScalarQuantizer::new(lower_quantile, upper_quantile, self.bits)?;
    let scalar_quantized_vector_similarity =
      ScalarQuantizedVectorSimilarity::from_vector_similarity(
        self.function,
        quantizer.get_constant_multiplier(),
        quantizer.bits,
      );
    let (query, vector) = self.quantized.split_at_mut(self.vectors[0].len());
    for i in 0..self.nearest_neighbors.len() {
      let query_correction = quantizer.quantize(&self.vectors[i], query, self.function);
      let score_docs_and_score_variance = &self.nearest_neighbors[i];
      let score_variance = score_docs_and_score_variance.score_variance;
      // calculate the score for the vector against its nearest neighbors but with quantized
      // scores now
      self.errors.reset();
      for score_doc in
        &score_docs_and_score_variance.score_docs[..score_docs_and_score_variance.score_docs_len]
      {
        let vector_correction =
          quantizer.quantize(&self.vectors[score_doc.doc as usize], vector, self.function);
        let q_score = scalar_quantized_vector_similarity.score(
          query,
          query_correction,
          vector,
          vector_correction,
        )?;
        self.errors.add((q_score - score_doc.score) as f64);
      }
      self
        .corr
        .add((1.0 - self.errors.var() / score_variance) as f64);
    }
    if self.corr.mean.is_nan() {
      Ok(0.0)
    } else {
      Ok(self.corr.mean)
    }
  }
}

/// Calculates and adjust the scores correctly for quantized vectors given the scalar quantization
/// parameters.
pub(crate) enum ScalarQuantizedVectorSimilarity {
  Euclidean { const_multiplier: f32 },
  DotProduct { const_multiplier: f32, bits: u8 },
  MaximumInnerProduct { const_multiplier: f32, bits: u8 },
}

impl ScalarQuantizedVectorSimilarity {
  /// Creates a [`ScalarQuantizedVectorSimilarity`] from a [`VectorSimilarityFunction`] and
  /// the constant multiplier used for quantization.
  ///
  /// - `sim`: similarity function
  /// - `const_multiplier`: constant multiplier used for quantization
  /// - `bits`: number of bits used for quantization
  ///
  /// Returns a [`ScalarQuantizedVectorSimilarity`] that applies the appropriate corrections.
  pub(crate) fn from_vector_similarity(
    sim: VectorSimilarityFunction,
    const_multiplier: f32,
    bits: u8,
  ) -> Self {
    match sim {
      VectorSimilarityFunction::Euclidean => Self::Euclidean { const_multiplier },
      VectorSimilarityFunction::Cosine | VectorSimilarityFunction::DotProduct => Self::DotProduct {
        const_multiplier,
        bits,
      },
      VectorSimilarityFunction::MaximumInnerProduct => Self::MaximumInnerProduct {
        const_multiplier,
        bits,
      },
    }
  }

  pub(crate) fn score(
    &self,
    query_vector: &[u8],
    query_vector_offset: f32,
    stored_vector: &[u8],
    vector_offset: f32,
  ) -> Result<f32> {
    match self {
      Self::Euclidean { const_multiplier } => {
        let square_distance = VECTOR_UTIL.square_distance_u8(stored_vector, query_vector)?;
        let adjusted_distance = square_distance as f32 * const_multiplier;
        Ok(1.0 / (1.0 + adjusted_distance))
      },
      Self::DotProduct {
        const_multiplier,
        bits,
      } => {
        let dot_product = dot_product_by_bits(stored_vector, query_vector, *bits)?;
        debug_assert!(dot_product >= 0);
        let adjusted_distance =
          dot_product as f32 * const_multiplier + query_vector_offset + vector_offset;
        Ok(CoreHelper::max_f32((1.0 + adjusted_distance) / 2.0, 0.0))
      },
      Self::MaximumInnerProduct {
        const_multiplier,
        bits,
      } => {
        let dot_product = dot_product_by_bits(stored_vector, query_vector, *bits)?;
        debug_assert!(dot_product >= 0);
        let adjusted_distance =
          dot_product as f32 * const_multiplier + query_vector_offset + vector_offset;
        Ok(VectorUtil::scale_max_inner_product_score(adjusted_distance))
      },
    }
  }
}

fn dot_product_by_bits(stored_vector: &[u8], query_vector: &[u8], bits: u8) -> Result<i32> {
  if bits <= 4 {
    VECTOR_UTIL.int4_dot_product(stored_vector, query_vector)
  } else {
    VECTOR_UTIL.dot_product_u8(stored_vector, query_vector)
  }
}

struct JavaRandom {
  seed: u64,
}

impl JavaRandom {
  const MULTIPLIER: u64 = 0x5DEECE66D;
  const ADDEND: u64 = 0xB;
  const MASK: u64 = (1u64 << 48) - 1;

  fn new(seed: i64) -> Self {
    Self {
      seed: ((seed as u64) ^ Self::MULTIPLIER) & Self::MASK,
    }
  }

  fn next(&mut self, bits: u32) -> i32 {
    self.seed = self
      .seed
      .wrapping_mul(Self::MULTIPLIER)
      .wrapping_add(Self::ADDEND)
      & Self::MASK;
    (self.seed >> (48 - bits)) as i32
  }

  fn next_int(&mut self, bound: i32) -> i32 {
    debug_assert!(bound > 0);
    if bound.isolate_lowest_one() == bound {
      return (((bound as i64) * (self.next(31) as i64)) >> 31) as i32;
    }
    loop {
      let bits = self.next(31);
      let value = bits % bound;
      if bits.wrapping_sub(value).wrapping_add(bound - 1) >= 0 {
        return value;
      }
    }
  }
}
