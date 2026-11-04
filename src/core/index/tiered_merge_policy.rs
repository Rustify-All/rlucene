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
use crate::core::index::index_writer::Inner;
use crate::core::index::merge_policy::{
  DEFAULT_MAX_CFS_SEGMENT_SIZE, DefaultMergeSpecification, MergeContext, MergePolicy,
  MergePolicyBase, MergeSpecification, OneMerge, assert_del_count, size,
};
use crate::core::index::merge_trigger::MergeTrigger;
use crate::core::index::segment_commit_info::SegmentCommitInfo;
use crate::core::index::segment_infos::SegmentInfos;
use crate::core::store::directory::Directory;
use crate::core::util::CoreHelper;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter};
use std::sync::Arc;

/// Default noCFSRatio. If a merge's size is >= 10% of the index, then we disable compound file for it.
pub const DEFAULT_NO_CFS_RATIO: f64 = 0.1;
/// Merges segments of approximately equal size, subject to an allowed number of segments per tier.
/// This is similar to [`LogByteSizeMergePolicy`](crate::core::index::log_byte_size_merge_policy::LogByteSizeMergePolicy), except this merge policy is able to merge
/// non-adjacent segment, and separates how many segments are merged at once
/// ([`TieredMergePolicy::set_max_merge_at_once`]) from how many segments are allowed per tier
/// ([`TieredMergePolicy::set_segments_per_tier`]). This merge policy also does not over-merge
/// (i.e. cascade merges).
///
/// For normal merging, this policy first computes a "budget" of how many segments are allowed to
/// be in the index. If the index is over-budget, then the policy sorts segments by decreasing size
/// (pro-rating by percent deletes), and then finds the least-cost merge. Merge cost is measured by a
/// combination of the "skew" of the merge (size of largest segment divided by smallest segment),
/// total merge size and percent deletes reclaimed, so that merges with lower skew, smaller size and
/// those reclaiming more deletes, are favored.
///
/// If a merge will produce a segment that's larger than
/// [`TieredMergePolicy::set_max_merged_segment_mb`], then the policy will merge fewer segments
/// (down to 1 at once, if that one has deletions) to keep the segment size under budget.
///
/// **NOTE**: this policy freely merges non-adjacent segments; if this is a problem, use
/// [`LogMergePolicy`](crate::core::index::log_merge_policy::LogMergePolicy).
///
/// **NOTE**: This policy always merges by byte size of the segments, always pro-rates by
/// percent deletes.
///
/// **NOTE** Starting with Lucene 7.5, if you call [`IndexWriter::force_merge`](crate::core::index::index_writer::IndexWriter::force_merge) with
/// this (default) merge policy, if [`TieredMergePolicy::set_max_merged_segment_mb`] is in conflict
/// with `maxNumSegments` passed to [`IndexWriter::force_merge`](crate::core::index::index_writer::IndexWriter::force_merge) then `maxNumSegments` wins. For
/// example, if your index has 50 1 GB segments, and you have
/// [`TieredMergePolicy::set_max_merged_segment_mb`] at 1024 (1 GB), and you call `force_merge(10)`,
/// the two settings are clearly in conflict. [`TieredMergePolicy`] will choose to break the
/// [`TieredMergePolicy::set_max_merged_segment_mb`] constraint and try to merge down to at most ten
/// segments, each up to 5 * 1.25 GB in size (since an extra 25% buffer increase in the expected
/// segment size is targetted).
///
/// findForcedDeletesMerges should never produce segments greater than maxSegmentSize.
///
/// **NOTE**: This policy returns natural merges whose size is below the
/// [`TieredMergePolicy::set_floor_segment_mb`] floor segment size for
/// [`TieredMergePolicy::find_full_flush_merges`] full-flush merges.
#[derive(Clone)]
pub struct TieredMergePolicy {
  // User-specified maxMergeAtOnce. In practice we always take the min of its
  // value and segsPerTier for segments above the floor size to avoid suboptimal merging.
  max_merge_at_once: i32,
  max_merged_segment_bytes: i64,
  floor_segment_bytes: i64,
  segs_per_tier: f64,
  force_merge_deletes_pct_allowed: f64,
  deletes_pct_allowed: f64,
  target_search_concurrency: i32,
  base: MergePolicyBase,
}
impl Default for TieredMergePolicy {
  fn default() -> Self {
    Self::new()
  }
}

impl TieredMergePolicy {
  pub fn new() -> Self {
    let base = MergePolicyBase::new(DEFAULT_NO_CFS_RATIO, DEFAULT_MAX_CFS_SEGMENT_SIZE);
    Self {
      max_merge_at_once: 10,
      max_merged_segment_bytes: 5 * 1024 * 1024 * 1024,
      floor_segment_bytes: 2 * 1024 * 1024,
      segs_per_tier: 10.0,
      force_merge_deletes_pct_allowed: 10.0,
      deletes_pct_allowed: 20.0,
      target_search_concurrency: 1,
      base,
    }
  }
  /// Maximum number of segments to be merged at a time during "normal" merging. Default is 10.
  ///
  /// **NOTE**: Merges above the [`TieredMergePolicy::set_floor_segment_mb`] floor segment size also
  /// bound the number of merged segments by [`TieredMergePolicy::set_segments_per_tier`] the number
  /// of segments per tier.
  pub fn set_max_merge_at_once(&mut self, v: i32) -> Result<&mut Self> {
    if v < 2 {
      return Err(LuceneError::illegal_argument(format!(
        "maxMergeAtOnce must be > 1 (got {})",
        v
      )));
    }
    self.max_merge_at_once = v;
    Ok(self)
  }

  /// Returns the current maxMergeAtOnce setting.
  pub fn get_max_merge_at_once(&self) -> i32 {
    self.max_merge_at_once
  }

  /// Maximum sized segment to produce during normal merging. This setting is approximate: the
  /// estimate of the merged segment size is made by summing sizes of to-be-merged segments
  /// (compensating for percent deleted docs). Default is 5 GB.
  pub fn set_max_merged_segment_mb(&mut self, mut v: f64) -> Result<&mut Self> {
    if v < 0.0 {
      return Err(LuceneError::illegal_argument(format!(
        "maxMergedSegmentMB must be >=0 (got {})",
        v
      )));
    }
    v *= 1024.0 * 1024.0;
    self.max_merged_segment_bytes = if v > i64::MAX as f64 {
      i64::MAX
    } else {
      v as i64
    };
    Ok(self)
  }
  /// Returns the current maxMergedSegmentMB setting.
  pub fn get_max_merged_segment_mb(&self) -> f64 {
    self.max_merged_segment_bytes as f64 / 1024.0 / 1024.0
  }

  /// Controls the maximum percentage of deleted documents that is tolerated in the index. Lower
  /// values make the index more space efficient at the expense of increased CPU and I/O activity.
  /// Values must be between 5 and 50. Default value is 20.
  ///
  /// When the maximum delete percentage is lowered, the indexing thread will call for merges more
  /// often, meaning that write amplification factor will be increased. Write amplification factor
  /// measures the number of times each document in the index is written. A higher write
  /// amplification factor will lead to higher CPU and I/O activity as indicated above.
  pub fn set_deletes_pct_allowed(&mut self, v: f64) -> Result<&mut Self> {
    // NaN passes this range check.
    if !v.is_nan() && !(5.0..=50.0).contains(&v) {
      return Err(LuceneError::illegal_argument(format!(
        "indexPctDeletedTarget must be >= 5.0 and <= 50 (got {})",
        v
      )));
    }
    self.deletes_pct_allowed = v;
    Ok(self)
  }

  /// Returns the current deletesPctAllowed setting.
  pub fn get_deletes_pct_allowed(&self) -> f64 {
    self.deletes_pct_allowed
  }

  /// Segments smaller than this size are merged more aggressively:
  ///
  /// - They are candidates for full-flush merges, in order to reduce the number of segments in
  ///   the index prior to opening a new point-in-time view of the index.
  /// - For background merges, smaller segments are "rounded up" to this size.
  ///
  /// In both cases, this helps prevent frequent flushing of tiny segments to create a long tail of
  /// small segments in the index. Default is 2MB.
  pub fn set_floor_segment_mb(&mut self, mut v: f64) -> Result<&mut Self> {
    if v <= 0.0 {
      return Err(LuceneError::illegal_argument(format!(
        "floorSegmentMB must be > 0.0 (got {})",
        v
      )));
    }
    v *= 1024.0 * 1024.0;
    self.floor_segment_bytes = if v > i64::MAX as f64 {
      i64::MAX
    } else {
      v as i64
    };
    Ok(self)
  }

  /// Returns the current floorSegmentMB.
  ///
  /// See also [`TieredMergePolicy::set_floor_segment_mb`].
  pub fn get_floor_segment_mb(&self) -> f64 {
    self.floor_segment_bytes as f64 / (1024.0 * 1024.0)
  }
  /// When forceMergeDeletes is called, we only merge away a segment if its delete percentage is over
  /// this threshold. Default is 10%.
  pub fn set_force_merge_deletes_pct_allowed(&mut self, v: f64) -> Result<&mut Self> {
    // NaN passes this range check.
    if !v.is_nan() && !(0.0..=100.0).contains(&v) {
      return Err(LuceneError::illegal_argument(format!(
        "forceMergeDeletesPctAllowed must be between 0.0 and 100.0 inclusive (got {})",
        v
      )));
    }
    self.force_merge_deletes_pct_allowed = v;
    Ok(self)
  }

  /// Returns the current forceMergeDeletesPctAllowed setting.
  ///
  /// See also [`TieredMergePolicy::set_force_merge_deletes_pct_allowed`].
  pub fn force_merge_deletes_pct_allowed(&self) -> f64 {
    self.force_merge_deletes_pct_allowed
  }

  /// Sets the allowed number of segments per tier. Smaller values mean more merging but fewer
  /// segments.
  ///
  /// Default is 10.0.
  pub fn set_segments_per_tier(&mut self, v: f64) -> Result<&mut Self> {
    if v < 2.0 {
      return Err(LuceneError::illegal_argument(format!(
        "segmentsPerTier must be >= 2.0 (got {})",
        v
      )));
    }
    self.segs_per_tier = v;
    Ok(self)
  }

  /// Returns the current segmentsPerTier setting.
  ///
  /// See also [`TieredMergePolicy::set_segments_per_tier`].
  pub fn get_segments_per_tier(&self) -> f64 {
    self.segs_per_tier
  }

  /// Sets the target search concurrency. This prevents creating segments that are bigger than
  /// maxDoc/targetSearchConcurrency, which in turn makes the work parallelizable into
  /// targetSearchConcurrency slices of similar doc counts. It also makes merging less aggressive,
  /// as higher values result in indices that do less merging and have more segments
  pub fn set_target_search_concurrency(
    &mut self,
    target_search_concurrency: i32,
  ) -> Result<&mut Self> {
    if target_search_concurrency < 1 {
      return Err(LuceneError::illegal_argument(format!(
        "targetSearchConcurrency must be >= 1 (got {})",
        target_search_concurrency
      )));
    }
    self.target_search_concurrency = target_search_concurrency;
    Ok(self)
  }

  /// Returns the target search concurrency.
  pub fn get_target_search_concurrency(&self) -> i32 {
    self.target_search_concurrency
  }
  // The size can change concurrently while we are running here, because deletes
  // are now applied concurrently, and this can piss off TimSort! So we
  // call size() once per segment and sort by that:
  fn get_sorted_by_segment_size<'a, D, MC>(
    &self,
    infos: &'a SegmentInfos<D>,
    merge_context: &MC,
  ) -> Result<Vec<SegmentSizeAndDocs<'a, D>>>
  where
    D: Directory,
    MC: MergeContext<D>,
  {
    let mut sorted_by_size = Vec::with_capacity(infos.size());

    for info in infos.iter() {
      sorted_by_size.push(SegmentSizeAndDocs::new(
        info,
        self.size(info, merge_context)?,
        merge_context.num_deletes_to_merge(info)?,
      )?);
    }

    sorted_by_size.sort_by(|o1, o2| {
      // Sort by largest size:
      let mut cmp = o2.size_in_bytes.cmp(&o1.size_in_bytes);
      if cmp == std::cmp::Ordering::Equal {
        cmp = o1.seg_info.info.name.cmp(&o2.seg_info.info.name);
      }
      cmp
    });

    Ok(sorted_by_size)
  }
  #[allow(clippy::too_many_arguments)]
  fn do_find_merges<MC, D>(
    &self,
    mut sorted_eligible: Vec<SegmentSizeAndDocs<'_, D>>,
    max_merged_segment_bytes: i64,
    merge_factor: i32,
    allowed_seg_count: usize,
    allowed_del_count: i32,
    allowed_doc_count: i32,
    merge_type: MergeType,
    merge_context: &MC,
    max_merge_is_running: bool,
  ) -> Result<Option<DefaultMergeSpecification<D>>>
  where
    MC: MergeContext<D>,
    D: Directory,
  {
    let mut seg_infos_sizes = HashMap::with_capacity(sorted_eligible.len());
    for seg in &sorted_eligible {
      seg_infos_sizes.insert(seg.seg_info.info.get_id_key(), *seg);
    }

    let original_sorted_size = sorted_eligible.len();
    if self.verbose(merge_context) {
      self.message(
        &format!("findMerges: {} segments", original_sorted_size),
        merge_context,
      )?;
    }
    if original_sorted_size == 0 {
      return Ok(None);
    }

    let mut to_be_merged = HashSet::new();
    to_be_merged.reserve(sorted_eligible.len());

    let mut spec = None;
    // Cycle to possibly select more than one merge:
    // The trigger point for total deleted documents in the index leads to a bunch of large segment
    // merges at the same time. So only put one large merge in the list of merges per cycle. We'll
    // pick up another
    // merge next time around.
    let mut have_one_large_merge = false;

    loop {
      // Gather eligible segments for merging, ie segments
      // not already being merged and not already picked (by
      // prior iteration of this loop) for merging:

      // Remove ineligible segments. These are either already being merged or already picked by
      // prior iterations
      sorted_eligible.retain(|s| !to_be_merged.contains(s.seg_info.info.get_id_key()));

      if self.verbose(merge_context) {
        self.message(
          &format!(
            "  allowedSegmentCount={} vs count={} (eligible count={})",
            allowed_seg_count,
            original_sorted_size,
            sorted_eligible.len()
          ),
          merge_context,
        )?;
      }

      if sorted_eligible.is_empty() {
        return Ok(spec);
      }

      let remaining_del_count: i32 = sorted_eligible.iter().map(|c| c.del_count).sum();
      if merge_type == MergeType::Natural
        && sorted_eligible.len() <= allowed_seg_count
        && remaining_del_count <= allowed_del_count
      {
        return Ok(spec);
      }
      // OK we are over budget -- find best merge!
      let mut best_score: Option<MergeScoreImpl> = None;
      let mut best = None;
      let mut best_too_large = false;
      let mut best_merge_bytes = 0;

      let mut candidate: smallvec::SmallVec<[SegmentCommitInfoMeta<'_, D>; 4]> =
        smallvec::SmallVec::new();
      for start_idx in 0..sorted_eligible.len() {
        candidate.clear();
        candidate.reserve_exact(std::cmp::min(
          self.max_merge_at_once as usize,
          sorted_eligible.len() - start_idx,
        ));
        let mut hit_too_large = false;
        let mut bytes_this_merge = 0;
        let mut doc_count_this_merge: i64 = 0;

        let mut idx = start_idx;
        while idx < sorted_eligible.len()
                    && candidate.len() < self.max_merge_at_once as usize
                    // We allow merging more than mergeFactor segments together if the merged segment
                    // would be less than the floor segment size. This is important because segments
                    // below the floor segment size are more aggressively merged by this policy, so we
                    // need to grow them as quickly as possible.
                    && (candidate.len() < merge_factor as usize
                    || bytes_this_merge < self.floor_segment_bytes)
                    && bytes_this_merge < max_merged_segment_bytes
                    && (bytes_this_merge < self.floor_segment_bytes
                    || doc_count_this_merge <= allowed_doc_count as i64)
        {
          let seg_size_docs = &sorted_eligible[idx];
          let seg_bytes = seg_size_docs.size_in_bytes;
          let seg_doc_count = seg_size_docs.max_doc - seg_size_docs.del_count;

          if bytes_this_merge + seg_bytes > max_merged_segment_bytes
            || (bytes_this_merge > self.floor_segment_bytes
              && doc_count_this_merge + seg_doc_count as i64 > allowed_doc_count as i64)
          {
            // Only set hitTooLarge when reaching the maximum byte size, as this will create
            // segments of the maximum size which will no longer be eligible for merging for a long
            // time (until they accumulate enough deletes).
            hit_too_large |= bytes_this_merge + seg_bytes > max_merged_segment_bytes;
            // We should never have something coming in that _cannot_ be merged, so handle
            // singleton merges
            if !candidate.is_empty() {
              // NOTE: we continue, so that we can try
              // "packing" smaller segments into this merge
              // to see if we can get closer to the max
              // size; this in general is not perfect since
              // this is really "bin packing" and we'd have
              // to try different permutations.
              idx += 1;
              continue;
            }
          }

          candidate.push(SegmentCommitInfoMeta::new(
            seg_size_docs.seg_info,
            seg_size_docs.size_in_seg,
            seg_size_docs.max_doc,
          ));
          bytes_this_merge += seg_bytes;
          doc_count_this_merge += seg_doc_count as i64;
          idx += 1;
        }
        // We should never see an empty candidate: we iterated over maxMergeAtOnce
        // segments, and already pre-excluded the too-large segments:
        debug_assert!(!candidate.is_empty());

        let max_candidate_segment_size =
          match seg_infos_sizes.get(candidate[0].seg_info.info.get_id_key()) {
            Some(c) => c,
            None => return Err(LuceneError::illegal_state("could not  find candidate")),
          };

        if !hit_too_large
          && merge_type == MergeType::Natural
          && (bytes_this_merge as f64) < max_candidate_segment_size.size_in_bytes as f64 * 1.5
          && (max_candidate_segment_size.del_count as f64)
            < max_candidate_segment_size.max_doc as f64 * self.deletes_pct_allowed / 100.0
        {
          // Ignore any merge where the resulting segment is not at least 50% larger than the
          // biggest input segment.
          // Otherwise we could run into pathological O(N^2) merging where merges keep rewriting
          // again and again the biggest input segment into a segment that is barely bigger.
          // The only error we make is when the merge would reclaim lots of deletes in the
          // biggest segment. This is important for cases when lots of documents get deleted at once
          // without introducing new segments of a similar size for instance.
          continue;
        }
        // A singleton merge with no deletes makes no sense. We can get here when forceMerge is
        // looping around...
        if candidate.len() == 1 && max_candidate_segment_size.del_count == 0 {
          continue;
        }
        // If we didn't find a too-large merge and have a list of candidates
        // whose length is less than the merge factor, it means we are reaching
        // the tail of the list of segments and will only find smaller merges.
        // Stop here.
        if best_score.is_some() && !hit_too_large && candidate.len() < merge_factor as usize {
          break;
        }

        let score = self.score(&candidate, hit_too_large, &seg_infos_sizes)?;
        if self.verbose(merge_context) {
          let mut candidate_segments = Vec::with_capacity(candidate.len());
          for meta in &candidate {
            let info = meta.seg_info;
            let del = merge_context.num_deleted_docs(info)? - info.get_del_count();
            candidate_segments.push(info.to_string_with_pending_del_count(del)?);
          }
          let candidate_string = candidate_segments.join(" ");
          self.message(
            &format!(
              "  maybe={} score={} {} tooLarge={} size={:.3} MB",
              candidate_string,
              score.score(),
              score.explanation(),
              hit_too_large,
              bytes_this_merge as f64 / 1024.0 / 1024.0
            ),
            merge_context,
          )?;
        }

        if best_score
          .as_ref()
          .is_none_or(|best_score| score.score() < best_score.score())
          && (!hit_too_large || !max_merge_is_running)
        {
          candidate = best.replace(candidate).unwrap_or_default();
          best_score = Some(score);
          best_too_large = hit_too_large;
          best_merge_bytes = bytes_this_merge;
        }
      }

      let best = match best {
        Some(b) => b,
        None => return Ok(spec),
      };
      // The mergeType == FORCE_MERGE_DELETES behaves as the code does currently and can create a
      // large number of
      // concurrent big merges. If we make findForcedDeletesMerges behave as findForcedMerges and
      // cycle through
      // we should remove this.
      if !have_one_large_merge || !best_too_large || merge_type == MergeType::ForceMergeDeletes {
        have_one_large_merge |= best_too_large;

        let spec_ref = spec.get_or_insert_with(MergeSpecification::new);
        let merge = OneMerge::from_meta(best.as_ref())?;
        spec_ref.add(merge);

        if self.verbose(merge_context) {
          let mut best_segments = Vec::with_capacity(best.len());
          for meta in &best {
            let info = meta.seg_info;
            let del = merge_context.num_deleted_docs(info)? - info.get_del_count();
            best_segments.push(info.to_string_with_pending_del_count(del)?);
          }
          let best_string = best_segments.join(" ");
          let best_score = best_score
            .as_ref()
            .ok_or_else(|| LuceneError::illegal_state("selected merge has no score"))?;
          self.message(
            &format!(
              "  add merge={} size={:.3} MB score={:.3} {}{}",
              best_string,
              best_merge_bytes as f64 / 1024.0 / 1024.0,
              best_score.score(),
              best_score.explanation(),
              if best_too_large { " [max merge]" } else { "" }
            ),
            merge_context,
          )?;
        }
      }
      // whether we're going to return this list in the spec of not, we need to remove it from
      // consideration on the next loop.
      for s in best {
        to_be_merged.insert(s.seg_info.info.get_id_key());
      }
    }
  }

  /// Expert: scores one merge; implementations may provide custom behavior.
  fn score<D>(
    &self,
    candidate: &[SegmentCommitInfoMeta<'_, D>],
    hit_too_large: bool,
    segments_sizes: &HashMap<&str, SegmentSizeAndDocs<'_, D>>,
  ) -> Result<MergeScoreImpl>
  where
    D: Directory,
  {
    let mut tot_before_merge_bytes: i64 = 0;
    let mut tot_after_merge_bytes: i64 = 0;
    let mut tot_after_merge_bytes_floored: i64 = 0;

    for info in candidate {
      let seg_bytes = segments_sizes
        .get(info.seg_info.info.get_id_key())
        .ok_or_else(|| LuceneError::illegal_state("candidate segment size is missing"))?
        .size_in_bytes;
      tot_after_merge_bytes += seg_bytes;
      tot_after_merge_bytes_floored += self.floor_size(seg_bytes);
      tot_before_merge_bytes += info.size_in_seg;
    }

    // Roughly measure "skew" of the merge, i.e. how
    // "balanced" the merge is (whether the segments are
    // about the same size), which can range from
    // 1.0/numSegsBeingMerged (good) to 1.0 (poor). Heavily
    // lopsided merges (skew near 1.0) is no good; it means
    // O(N^2) merge cost over time:
    let skew: f64 = if hit_too_large {
      // Pretend the merge has perfect skew; skew doesn't
      // matter in this case because this merge will not
      // "cascade" and so it cannot lead to N^2 merge cost
      // over time:
      let merge_factor = std::cmp::min(self.max_merge_at_once, self.segs_per_tier as i32);
      1.0 / (merge_factor as f64)
    } else {
      (self.floor_size(
        segments_sizes
          .get(candidate[0].seg_info.info.get_id_key())
          .ok_or_else(|| LuceneError::illegal_state("candidate segment size is missing"))?
          .size_in_bytes,
      ) as f64)
        / (tot_after_merge_bytes_floored as f64)
    };

    // Strongly favor merges with less skew (smaller
    // mergeScore is better):
    let mut merge_score = skew;

    // Gently favor smaller merges over bigger ones. We
    // don't want to make this exponent too large else we
    // can end up doing poor merges of small segments in
    // order to avoid the large merges:
    merge_score *= (tot_after_merge_bytes as f64).powf(0.05);

    // Strongly favor merges that reclaim deletes:
    let non_del_ratio = (tot_after_merge_bytes as f64) / (tot_before_merge_bytes as f64);
    merge_score *= non_del_ratio.powf(2.0);

    let final_merge_score = merge_score;

    Ok(MergeScoreImpl {
      final_merge_score,
      skew,
      non_del_ratio,
    })
  }
  pub(crate) fn get_max_allowed_docs(&self, total_max_doc: i32, total_del_docs: i32) -> i32 {
    let v = total_max_doc - total_del_docs;
    v / self.target_search_concurrency + i32::from(v % self.target_search_concurrency > 0)
  }

  fn floor_size(&self, bytes: i64) -> i64 {
    std::cmp::max(self.floor_segment_bytes, bytes)
  }
}
pub struct SegmentCommitInfoMeta<'a, D> {
  pub(crate) seg_info: &'a SegmentCommitInfo<D>,
  pub(crate) size_in_seg: i64,
  pub(crate) max_doc: i32,
}
impl<'a, D> SegmentCommitInfoMeta<'a, D> {
  fn new(seg_info: &'a SegmentCommitInfo<D>, size_in_seg: i64, max_doc: i32) -> Self {
    Self {
      seg_info,
      size_in_seg,
      max_doc,
    }
  }
}
#[derive(Clone)]
pub struct SegmentDocAndID {
  pub(crate) seg_id: Arc<str>,
  pub(crate) max_doc: i32,
}
impl SegmentDocAndID {
  pub(crate) fn new<S: Into<Arc<str>>>(seg_id: S, max_doc: i32) -> Self {
    Self {
      seg_id: seg_id.into(),
      max_doc,
    }
  }
}

impl Display for TieredMergePolicy {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(
      f,
      "[{}: maxMergeAtOnce={}, maxMergedSegmentMB={}, floorSegmentMB={}, \
forceMergeDeletesPctAllowed={}, segmentsPerTier={}, maxCFSSegmentSizeMB={}, \
noCFSRatio={}, deletesPctAllowed={}, targetSearchConcurrency={}",
      std::any::type_name::<Self>()
        .rsplit("::")
        .next()
        .unwrap_or("TieredMergePolicy"),
      self.max_merge_at_once,
      self.max_merged_segment_bytes as f64 / 1024.0 / 1024.0,
      self.floor_segment_bytes as f64 / 1024.0 / 1024.0,
      self.force_merge_deletes_pct_allowed,
      self.segs_per_tier,
      self.base.get_max_cfs_segment_size_mb(),
      self.base.get_no_cfs_ratio(),
      self.deletes_pct_allowed,
      self.target_search_concurrency,
    )
  }
}

impl<D> MergePolicy<D> for TieredMergePolicy
where
  D: Directory,
{
  fn get_base(&self) -> &MergePolicyBase {
    &self.base
  }

  fn get_base_mut(&mut self) -> &mut MergePolicyBase {
    &mut self.base
  }

  fn find_merges<MC>(
    &self,
    _merge_trigger: MergeTrigger,
    infos: &SegmentInfos<D>,
    inner: Option<&Inner<D>>,
    merge_context: &MC,
  ) -> Result<Option<DefaultMergeSpecification<D>>>
  where
    MC: MergeContext<D>,
  {
    // Compute total index bytes & print details about the index
    let mut tot_index_bytes: i64 = 0;
    let mut min_segment_bytes: i64 = i64::MAX;

    let mut total_del_docs: i32 = 0;
    let mut total_max_doc: i32 = 0;

    let mut merging_bytes: i64 = 0;

    let merging = merge_context.get_merging_segments(inner);
    let mut sorted_infos = self.get_sorted_by_segment_size(infos, merge_context)?;
    if self.verbose(merge_context) {
      for seg in &sorted_infos {
        let mut extra = if merging.contains(seg.seg_info.info.get_id_key()) {
          " [merging]".to_string()
        } else {
          String::new()
        };
        if seg.size_in_bytes >= self.max_merged_segment_bytes {
          extra.push_str(" [skip: too large]");
        } else if seg.size_in_bytes < self.floor_segment_bytes {
          extra.push_str(" [floored]");
        }
        self.message(
          &format!(
            "  seg={} size={:.3} MB{}",
            self.seg_string(merge_context, std::slice::from_ref(seg.seg_info))?,
            seg.size_in_bytes as f64 / 1024.0 / 1024.0,
            extra
          ),
          merge_context,
        )?;
      }
    }
    sorted_infos.retain(|seg| {
      let seg_bytes = seg.size_in_bytes;

      min_segment_bytes = std::cmp::min(seg_bytes, min_segment_bytes);
      tot_index_bytes += seg_bytes;

      if merging.contains(seg.seg_info.info.get_id_key()) {
        merging_bytes += seg_bytes;
        // if this segment is merging, then its deletes are being reclaimed already.
        // only count live docs in the total max doc
        total_max_doc += seg.max_doc - seg.del_count;
        false
      } else {
        total_del_docs += seg.del_count;
        total_max_doc += seg.max_doc;
        true
      }
    });

    debug_assert!(total_max_doc >= 0);
    debug_assert!(total_del_docs >= 0);

    let total_del_pct = 100.0 * total_del_docs as f64 / total_max_doc as f64;
    let mut allowed_del_count = (self.deletes_pct_allowed * total_max_doc as f64 / 100.0) as i32;
    // If we have too-large segments, grace them out of the maximum segment count
    // If we're above certain thresholds of deleted docs, we can merge very large segments.
    let mut too_big_count = 0;
    // We relax merging for the bigger segments for concurrency reasons, as we want to have several
    // segments on the highest tier without over-merging on the lower tiers.
    let mut concurrency_count = 0;
    let mut allowed_seg_count: f64 = 0.0;
    // remove large segments from consideration under two conditions.
    // 1> Overall percent deleted docs relatively small and this segment is larger than 50%
    // maxSegSize
    // 2> overall percent deleted docs large and this segment is large and has few deleted docs
    sorted_infos.retain(|seg| {
      let seg_del_pct = 100.0 * seg.del_count as f64 / seg.max_doc as f64;

      if seg.size_in_bytes > self.max_merged_segment_bytes / 2
        && (total_del_pct <= self.deletes_pct_allowed || seg_del_pct <= self.deletes_pct_allowed)
      {
        too_big_count += 1;
        tot_index_bytes -= seg.size_in_bytes;
        allowed_del_count -= seg.del_count;
        false
      } else if concurrency_count + too_big_count < self.target_search_concurrency - 1 {
        // Make sure we count a whole segment for the first targetSearchConcurrency-1 segments to
        // avoid over merging on the lower levels.
        concurrency_count += 1;
        allowed_seg_count += 1.0;
        tot_index_bytes -= seg.size_in_bytes;
        true
      } else {
        true
      }
    });

    allowed_del_count = std::cmp::max(0, allowed_del_count);

    let merge_factor = std::cmp::min(self.max_merge_at_once, self.segs_per_tier as i32);
    // Compute max allowed segments for the remainder of the index
    let mut level_size = std::cmp::max(min_segment_bytes, self.floor_segment_bytes);
    let mut bytes_left = tot_index_bytes;

    loop {
      let seg_count_level = bytes_left as f64 / level_size as f64;
      if seg_count_level < self.segs_per_tier || level_size == self.max_merged_segment_bytes {
        allowed_seg_count += seg_count_level.ceil();
        break;
      }
      allowed_seg_count += self.segs_per_tier;
      bytes_left = (bytes_left as f64 - self.segs_per_tier * level_size as f64) as i64;
      level_size = std::cmp::min(
        self.max_merged_segment_bytes,
        level_size * merge_factor as i64,
      );
    }
    // allowedSegCount may occasionally be less than segsPerTier
    // if segment sizes are below the floor size

    allowed_seg_count = CoreHelper::max_f64(allowed_seg_count, self.segs_per_tier);
    // No need to merge if the total number of segments (including too big segments) is less than or
    // equal to the target search concurrency.
    allowed_seg_count = CoreHelper::max_f64(
      allowed_seg_count,
      (self.target_search_concurrency - too_big_count) as f64,
    );

    let allowed_doc_count = self.get_max_allowed_docs(total_max_doc, total_del_docs);
    if self.verbose(merge_context) && too_big_count > 0 {
      self.message(
        &format!("  allowedSegmentCount={} vs count={} (eligible count={}) tooBigCount= {}  allowedDocCount={} vs doc count={}",
          allowed_seg_count, infos.size(), sorted_infos.len(), too_big_count, allowed_doc_count, infos.total_max_doc()?),
        merge_context,
      )?;
    }

    self.do_find_merges(
      sorted_infos,
      self.max_merged_segment_bytes,
      merge_factor,
      allowed_seg_count as usize,
      allowed_del_count,
      allowed_doc_count,
      MergeType::Natural,
      merge_context,
      merging_bytes >= self.max_merged_segment_bytes,
    )
  }

  fn find_forced_merges<MC>(
    &self,
    infos: &SegmentInfos<D>,
    max_segment_count: usize,
    segments_to_merge: &HashMap<String, Option<bool>>,
    inner: Option<&Inner<D>>,
    merge_context: &MC,
  ) -> Result<Option<DefaultMergeSpecification<D>>>
  where
    MC: MergeContext<D>,
  {
    if self.verbose(merge_context) {
      self.message(
        &format!(
          "findForcedMerges maxSegmentCount={} infos={} segmentsToMerge={:?}",
          max_segment_count,
          self.seg_string(merge_context, infos.iter())?,
          segments_to_merge
        ),
        merge_context,
      )?;
    }
    let mut sorted_size_and_docs = self.get_sorted_by_segment_size(infos, merge_context)?;

    let mut total_merge_bytes: i64 = 0;
    let merging = merge_context.get_merging_segments(inner);
    let mut force_merge_running = false;
    // Trim the list down, remove if we're respecting max segment size and it's not original.
    // Presumably it's been merged before and is close enough to the max segment size we
    // shouldn't add it in again.
    sorted_size_and_docs.retain(|seg| {
      let seg_id = seg.seg_info.info.get_id_key();
      let is_original = segments_to_merge.get(seg_id).copied();
      if let Some(Some(_)) = is_original {
        if merging.contains(seg_id) {
          force_merge_running = true;
          false
        } else {
          total_merge_bytes += seg.size_in_bytes;
          true
        }
      } else {
        false
      }
    });

    let mut max_merge_bytes = self.max_merged_segment_bytes;

    // Set the maximum segment size based on how many segments have been specified.
    if max_segment_count == 1 {
      max_merge_bytes = i64::MAX;
    } else if max_segment_count != i32::MAX as usize {
      max_merge_bytes = std::cmp::max(
        ((total_merge_bytes as f64) / (max_segment_count as f64)) as i64,
        self.max_merged_segment_bytes,
      );
      // Fudge this up a bit so we have a better chance of not having to do a second pass of merging
      // to get
      // down to the requested target segment count. If we use the exact size, it's almost
      // guaranteed
      // that the segments selected below won't fit perfectly and we'll be left with more segments
      // than
      // we want and have to re-merge in the code at the bottom of this method.
      max_merge_bytes = (max_merge_bytes as f64 * 1.25) as i64;
    }

    let mut found_deletes = false;

    sorted_size_and_docs.retain(|seg| {
      let is_original = segments_to_merge
        .get(seg.seg_info.info.get_id_key())
        .copied();

      if seg.del_count != 0 {
        // This is forceMerge; all segments with deleted docs should be merged.
        if matches!(is_original, Some(Some(true))) {
          found_deletes = true;
        }
        return true;
      }

      // Let the scoring handle whether to merge large segments.
      if max_segment_count == i32::MAX as usize && matches!(is_original, Some(Some(false))) {
        return false;
      }

      // Don't try to merge a segment with no deleted docs that's over the max size.
      if max_segment_count != i32::MAX as usize && seg.size_in_bytes >= max_merge_bytes {
        return false;
      }

      true
    });

    // Nothing to merge this round.
    if sorted_size_and_docs.is_empty() {
      return Ok(None);
    }
    let sorted_size_and_docs_len = sorted_size_and_docs.len();
    // We only bail if there are no deletions
    if !found_deletes {
      let info_zero = sorted_size_and_docs[0].seg_info;
      let info_zero_id = info_zero.info.get_id_key();

      let already = if max_segment_count != i32::MAX as usize
        && max_segment_count > 1
        && sorted_size_and_docs_len <= max_segment_count
      {
        true
      } else {
        max_segment_count == 1
          && sorted_size_and_docs_len == 1
          && (segments_to_merge.get(info_zero_id).is_some()
            || self.has_merged(infos, info_zero, merge_context)?)
      };

      if already {
        if self.verbose(merge_context) {
          self.message("already merged", merge_context)?;
        }
        return Ok(None);
      }
    }

    if self.verbose(merge_context) {
      let mut eligible = Vec::with_capacity(sorted_size_and_docs.len());
      for seg in &sorted_size_and_docs {
        eligible.push(seg.seg_info.info.name.as_ref());
      }
      self.message(&format!("eligible={:?}", eligible), merge_context)?;
    }
    let starting_segment_count = sorted_size_and_docs.len();
    if force_merge_running {
      // hmm this is a little dangerous -- if a user kicks off a forceMerge, it is taking forever,
      // lots of
      // new indexing/segments happened since, and they want to kick off another to ensure those
      // newly
      // indexed segments partake in the force merge, they (silently) won't due to this?
      return Ok(None);
    }

    // This is the special case of merging down to one segment
    if max_segment_count == 1 && total_merge_bytes < max_merge_bytes {
      let mut spec = DefaultMergeSpecification::new();
      spec.add(OneMerge::new(sorted_size_and_docs.iter().map(|s| {
        SegmentDocAndID::new(s.seg_info.info.id_key.clone(), s.max_doc)
      }))?);
      return Ok(Some(spec));
    }

    let mut spec: Option<DefaultMergeSpecification<D>> = None;

    let mut index = starting_segment_count.checked_sub(1);
    let mut resulting_segments = starting_segment_count;
    let mut candidate: smallvec::SmallVec<[SegmentCommitInfoMeta<'_, D>; 4]> =
      smallvec::SmallVec::with_capacity(starting_segment_count);

    loop {
      candidate.clear();
      let mut current_candidate_bytes: i64 = 0;

      while let Some(current_index) = index {
        if resulting_segments <= max_segment_count {
          break;
        }
        let sorted_size_and_doc = &sorted_size_and_docs[current_index];
        let initial_candidate_size = candidate.len();
        let current_segment_size = sorted_size_and_doc.size_in_seg;
        // We either add to the bin because there's space or because the it is the smallest possible
        // bin since
        // decrementing the index will move us to even larger segments.
        if current_candidate_bytes + current_segment_size <= max_merge_bytes
          || initial_candidate_size < 2
        {
          candidate.push(SegmentCommitInfoMeta::new(
            sorted_size_and_doc.seg_info,
            sorted_size_and_doc.size_in_seg,
            sorted_size_and_doc.max_doc,
          ));
          index = current_index.checked_sub(1);
          current_candidate_bytes += current_segment_size;
          if initial_candidate_size > 0 {
            // Any merge that handles two or more segments reduces the resulting number of segments
            // by the number of segments handled - 1
            resulting_segments -= 1;
          }
        } else {
          break;
        }
      }

      let candidate_size = candidate.len();

      // While a force merge is running, only merges that cover the maximum allowed number of
      // segments or that create a segment close to the
      // maximum allowed segment sized are permitted
      if candidate_size > 1
        && (!force_merge_running
          || (current_candidate_bytes as f64) > 0.7 * (max_merge_bytes as f64))
      {
        let merge = OneMerge::from_meta(candidate.as_ref())?;
        if self.verbose(merge_context) {
          let mut candidate_segments = Vec::with_capacity(candidate.len());
          for meta in &candidate {
            let info = meta.seg_info;
            let del = merge_context.num_deleted_docs(info)? - info.get_del_count();
            candidate_segments.push(info.to_string_with_pending_del_count(del)?);
          }
          self.message(
            &format!("add merge={}", candidate_segments.join(" ")),
            merge_context,
          )?;
        }

        let spec_ref = spec.get_or_insert_with(DefaultMergeSpecification::new);
        spec_ref.add(merge);
      } else {
        return Ok(spec);
      }
    }
  }

  fn find_forced_deletes_merges<MC>(
    &self,
    infos: &SegmentInfos<D>,
    inner: Option<&Inner<D>>,
    merge_context: &MC,
  ) -> Result<Option<DefaultMergeSpecification<D>>>
  where
    MC: MergeContext<D>,
  {
    if self.verbose(merge_context) {
      self.message(
        &format!(
          "findForcedDeletesMerges infos={} forceMergeDeletesPctAllowed={}",
          self.seg_string(merge_context, infos.iter())?,
          self.force_merge_deletes_pct_allowed
        ),
        merge_context,
      )?;
    }
    let merging = merge_context.get_merging_segments(inner);
    // First do a quick check that there's any work to do.
    // NOTE: this makes BaseMergePOlicyTestCase.testFindForcedDeletesMerges work
    let mut have_work = false;
    let mut total_del_count: i32 = 0;

    for info in infos.iter() {
      let del_count = merge_context.num_deletes_to_merge(info)?;
      debug_assert!(assert_del_count(del_count, info)?);
      total_del_count += del_count;

      let pct_deletes = 100.0 * (del_count as f64) / (info.info.max_doc()? as f64);
      have_work = have_work
        || (pct_deletes > self.force_merge_deletes_pct_allowed
          && !merging.contains(info.info.get_id_key()));
    }

    if !have_work {
      return Ok(None);
    }

    let mut sorted_infos = self.get_sorted_by_segment_size(infos, merge_context)?;

    sorted_infos.retain(|seg| {
      let pct_deletes = 100.0 * (seg.del_count as f64 / seg.max_doc as f64);
      !(merging.contains(seg.seg_info.info.get_id_key())
        || pct_deletes <= self.force_merge_deletes_pct_allowed)
    });

    if self.verbose(merge_context) {
      let mut eligible = Vec::with_capacity(sorted_infos.len());
      for seg in &sorted_infos {
        eligible.push(seg.seg_info.info.name.as_ref());
      }
      self.message(&format!("eligible={:?}", eligible), merge_context)?;
    }
    self.do_find_merges(
      sorted_infos,
      self.max_merged_segment_bytes,
      i32::MAX,
      usize::MAX,
      0,
      self.get_max_allowed_docs(infos.total_max_doc()?, total_del_count),
      MergeType::ForceMergeDeletes,
      merge_context,
      false,
    )
  }

  fn size<MC>(&self, info: &SegmentCommitInfo<D>, merge_context: &MC) -> Result<i64>
  where
    MC: MergeContext<D>,
  {
    size(info, merge_context)
  }

  fn max_full_flush_merge_size(&self) -> i64 {
    self.floor_segment_bytes
  }
}
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum MergeType {
  Natural,
  #[allow(dead_code)]
  ForceMerge,
  ForceMergeDeletes,
}
struct SegmentSizeAndDocs<'a, D> {
  seg_info: &'a SegmentCommitInfo<D>,
  /// Size of the segment in bytes, pro-rated by the number of live documents.
  size_in_bytes: i64,
  size_in_seg: i64,
  del_count: i32,
  max_doc: i32,
}

impl<D> Copy for SegmentSizeAndDocs<'_, D> {}

impl<D> Clone for SegmentSizeAndDocs<'_, D> {
  fn clone(&self) -> Self {
    *self
  }
}

impl<'a, D> SegmentSizeAndDocs<'a, D>
where
  D: Directory,
{
  fn new(info: &'a SegmentCommitInfo<D>, size_in_bytes: i64, seg_del_count: i32) -> Result<Self> {
    let max_doc = info.info.max_doc()?;
    Ok(Self {
      seg_info: info,
      size_in_bytes,
      size_in_seg: info.size_in_bytes()?,
      del_count: seg_del_count,
      max_doc,
    })
  }
}
/// Holds score and explanation for a single candidate merge.
pub(crate) trait MergeScore {
  /// Returns the score for this merge candidate; lower scores are better.
  fn score(&self) -> f64;

  /// Human readable explanation of how the merge got this score.
  fn explanation(&self) -> String;
}
struct MergeScoreImpl {
  final_merge_score: f64,
  skew: f64,
  non_del_ratio: f64,
}

impl MergeScore for MergeScoreImpl {
  fn score(&self) -> f64 {
    self.final_merge_score
  }

  fn explanation(&self) -> String {
    format!(
      "skew={:.3} nonDelRatio={:.3}",
      self.skew, self.non_del_ratio
    )
  }
}
