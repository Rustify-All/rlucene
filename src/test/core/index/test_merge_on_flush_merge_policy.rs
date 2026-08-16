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
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::index_writer::SOURCE_FLUSH;
use crate::core::index::merge_policy::{MergePolicy, MergePolicyEnum, MergeSpecification};
use crate::core::index::merge_trigger::MergeTrigger;
use crate::core::index::segment_commit_info::SegmentCommitInfo;
use crate::core::index::segment_infos::SegmentInfos;
use crate::core::store::directory::Directory;
use crate::core::util::LATEST;
use crate::core::util::error::lucene_error::Result;
use crate::sandbox::index::merge_on_flush_merge_policy::{MergeOnFlushMergePolicy, Units};
use crate::test_framework::core::index::base_merge_policy_test_case::{
  BaseMergePolicyTestCase, FakeDirectory, MockMergeContext, make_segment_commit_info,
};
use crate::test_framework::core::util::lucene_test_case::{new_merge_policy, random};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::prelude::StdRng;
use rand::{Rng, RngExt};
use std::collections::HashSet;
use std::sync::Arc;

struct TestMergeOnFlushMergePolicy;

fn run_case<F>(f: F) -> Result<()>
where
  F: FnOnce(&TestMergeOnFlushMergePolicy, &mut StdRng) -> Result<()>,
{
  let mut random = random();
  let case = TestMergeOnFlushMergePolicy;
  f(&case, &mut random)
}

impl BaseMergePolicyTestCase for TestMergeOnFlushMergePolicy {
  type MergePolicy<D>
    = MergePolicyEnum<D>
  where
    D: Directory;

  fn merge_policy<D, R>(&self, random: &mut R) -> Self::MergePolicy<D>
  where
    D: Directory,
    R: Rng + ?Sized,
  {
    let mut inner = new_merge_policy(random).expect("");
    let max_cfs_segment_size_mb = inner.get_base().get_max_cfs_segment_size_mb();
    let no_cfs_ratio = inner.get_base().get_no_cfs_ratio();
    let small_segment_threshold_mb = TestUtil::next_int(random, 1, 100) as f64;
    if let MergePolicyEnum::Tiered(tiered) = &mut inner {
      tiered
        .set_max_merged_segment_mb(TestUtil::next_int(random, 1024, 10 * 1024) as f64)
        .expect("");
    }

    let mut merge_on_flush = MergeOnFlushMergePolicy::new(inner);
    merge_on_flush
      .get_base_mut()
      .set_max_cfs_segment_size_mb(max_cfs_segment_size_mb)
      .expect("");
    merge_on_flush
      .get_base_mut()
      .set_no_cfs_ratio(no_cfs_ratio)
      .expect("");
    merge_on_flush.set_small_segment_threshold_mb(small_segment_threshold_mb);
    merge_on_flush.into()
  }

  fn assert_segment_infos<D>(_policy: &Self::MergePolicy<D>, _infos: &SegmentInfos<D>) -> Result<()>
  where
    D: Directory,
  {
    Ok(())
  }

  fn assert_merge<D, CR>(
    _policy: &Self::MergePolicy<D>,
    _merge: &MergeSpecification<D, CR>,
  ) -> Result<()>
  where
    D: Directory,
    CR: CodecReader,
  {
    Ok(())
  }
}

#[test]
fn test_find_full_flush_merges() -> Result<()> {
  let mut random = random();
  let merge_policy = match TestMergeOnFlushMergePolicy.merge_policy(&mut random) {
    MergePolicyEnum::MergeOnFlush(mp) => mp,
    _ => unreachable!(),
  };
  let small_segment_threshold_mb = merge_policy.get_small_segment_threshold_mb();
  let fake_directory = Arc::new(FakeDirectory::new());

  for _ in 0..10_000 {
    let mut segment_infos = SegmentInfos::new(LATEST.major)?;
    let num_segs = random.random_range(0..50);
    let mut merging_segments = HashSet::new();
    let mut small_segments = HashSet::new();

    for i in 0..num_segs {
      let max_doc = TestUtil::next_int(&mut random, 10, 100);
      let del_count = random.random_range(0..10);
      let size_mb = random.random::<f64>() * 2.0 * small_segment_threshold_mb;
      let sci = make_segment_commit_info(
        &mut random,
        fake_directory.clone(),
        &format!("_{}", i),
        max_doc,
        del_count,
        size_mb,
        SOURCE_FLUSH,
      )?;
      let seg_key = sci.info.get_id_key().to_string();

      if sci.size_in_bytes()? < Units::mb_to_bytes(small_segment_threshold_mb) {
        small_segments.insert(seg_key.clone());
      }
      if random.random_bool(0.5) {
        merging_segments.insert(seg_key);
      }
      segment_infos.add(sci)?;
    }

    let mut merge_context =
      MockMergeContext::new(|s: &SegmentCommitInfo<_>| Ok(s.get_del_count()));
    merge_context.set_merging_segments(merging_segments.clone());

    let merge_spec = merge_policy.find_full_flush_merges(
      MergeTrigger::Commit,
      &segment_infos,
      None,
      &merge_context,
    )?;

    if let Some(merge_spec) = merge_spec {
      for one_merge in &merge_spec.merges {
        for seg_key in &one_merge.stat.segments {
          assert!(small_segments.contains(seg_key));
          assert!(!merging_segments.contains(seg_key));
        }
      }
    } else {
      let mut found_non_merging_small_segment = false;
      for small_segment in &small_segments {
        if !merging_segments.contains(small_segment) {
          assert!(!found_non_merging_small_segment);
          found_non_merging_small_segment = true;
        }
      }
    }
  }

  Ok(())
}

#[test]
fn test_force_merge_not_needed() -> Result<()> {
  run_case(|case, random| case.test_force_merge_not_needed(random))
}

#[test]
fn test_find_forced_deletes_merges() -> Result<()> {
  run_case(|case, random| case.test_find_forced_deletes_merges(random))
}

#[test]
fn test_simulate_append_only() -> Result<()> {
  run_case(|case, random| {
    let mp = case.merge_policy(random);
    case.test_simulate_append_only(random, &mp, Arc::new(FakeDirectory::new()))
  })
}

#[test]
fn test_simulate_updates() -> Result<()> {
  run_case(|case, random| {
    let mp = case.merge_policy(random);
    case.test_simulate_updates(random, &mp, Arc::new(FakeDirectory::new()))
  })
}

#[test]
fn test_no_pathological_merges() -> Result<()> {
  Ok(())
}
