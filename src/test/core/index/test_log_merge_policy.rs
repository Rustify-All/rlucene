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
use crate::core::index::index_writer::{SOURCE_FLUSH, SOURCE_MERGE};
use crate::core::index::log_byte_size_merge_policy::LogByteSizeMergePolicy;
use crate::core::index::log_doc_merge_policy::LogDocMergePolicy;
use crate::core::index::log_merge_policy::LogMergePolicy;
use crate::core::index::merge_policy::{
  DefaultMergeSpecification, MergePolicy, MergePolicyEnum, MergeSpecification,
};
use crate::core::index::merge_trigger::MergeTrigger;
use crate::core::index::segment_commit_info::SegmentCommitInfo;
use crate::core::index::segment_infos::SegmentInfos;
use crate::core::store::directory::Directory;
use crate::core::util::LATEST;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::test_framework::core::index::base_merge_policy_test_case::{
  BaseMergePolicyTestCase, FakeDirectory, IOStats, MockMergeContext, apply_merge,
  make_segment_commit_info,
};
use crate::test_framework::core::util::lucene_test_case::{new_log_merge_policy, random};
use rand::Rng;
use rand::prelude::StdRng;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

struct TestLogMergePolicy;

fn run_case<F>(f: F) -> Result<()>
where
  F: FnOnce(&TestLogMergePolicy, &mut StdRng) -> Result<()>,
{
  let mut random = random();
  let case = TestLogMergePolicy;
  f(&case, &mut random)
}

impl BaseMergePolicyTestCase for TestLogMergePolicy {
  type MergePolicy<D>
    = MergePolicyEnum<D>
  where
    D: Directory;

  fn merge_policy<D, R>(&self, random: &mut R) -> Self::MergePolicy<D>
  where
    D: Directory,
    R: Rng + ?Sized,
  {
    new_log_merge_policy(random).expect("")
  }

  fn assert_segment_infos<D>(policy: &Self::MergePolicy<D>, infos: &SegmentInfos<D>) -> Result<()>
  where
    D: Directory,
  {
    let merge_context = MockMergeContext::new(|s: &SegmentCommitInfo<D>| Ok(s.get_del_count()));
    match policy {
      MergePolicyEnum::LogDoc(mp) => {
        for info in infos.iter() {
          assert!(
            mp.size(info, &merge_context)? / (mp.get_merge_factor() as i64) < mp.max_merge_size
          );
        }
        Ok(())
      },
      MergePolicyEnum::LogBytesSize(mp) => {
        for info in infos.iter() {
          assert!(
            mp.size(info, &merge_context)? / (mp.get_merge_factor() as i64) < mp.max_merge_size
          );
        }
        Ok(())
      },
      _ => Err(LuceneError::illegal_state(
        "expected LogMergePolicy variant",
      )),
    }
  }

  fn assert_merge<D, CR>(
    policy: &Self::MergePolicy<D>,
    merge: &MergeSpecification<D, CR>,
  ) -> Result<()>
  where
    D: Directory,
    CR: CodecReader,
  {
    match policy {
      MergePolicyEnum::LogDoc(mp) => {
        for one_merge in &merge.merges {
          assert!(one_merge.stat.segments.len() <= mp.get_merge_factor());
        }
        Ok(())
      },
      MergePolicyEnum::LogBytesSize(mp) => {
        for one_merge in &merge.merges {
          assert!(one_merge.stat.segments.len() <= mp.get_merge_factor());
        }
        Ok(())
      },
      _ => Err(LuceneError::illegal_state(
        "expected LogMergePolicy variant",
      )),
    }
  }
}

#[test]
fn test_default_forced_merge_mb() {
  let mp = LogMergePolicy::<LogByteSizeMergePolicy>::log_bytes_size();
  assert!(mp.get_max_merge_mb_for_forced_merge() > 0.0);
}

#[test]
fn test_increasing_segment_sizes() -> Result<()> {
  let mut r = random();
  let merge_policy = LogMergePolicy::<LogDocMergePolicy>::log_doc();
  let mut stats = IOStats::default();
  let seg_name_generator = AtomicU64::new(0);
  let merge_context = MockMergeContext::new(|s: &SegmentCommitInfo<_>| Ok(s.get_del_count()));
  let fake_directory = Arc::new(FakeDirectory::new());
  let mut segment_infos = SegmentInfos::new(LATEST.major)?;

  for i in 0..11 {
    segment_infos.add(make_segment_commit_info(
      &mut r,
      fake_directory.clone(),
      &format!("_{}", seg_name_generator.fetch_add(1, Ordering::SeqCst)),
      (i + 1) * 1000,
      0,
      0.0,
      SOURCE_MERGE,
    )?)?;
  }

  let spec_opt: Option<DefaultMergeSpecification<FakeDirectory>> =
    merge_policy.find_merges(MergeTrigger::Explicit, &segment_infos, None, &merge_context)?;
  assert!(spec_opt.is_some());
  let spec = spec_opt.unwrap();

  for one_merge in &spec.merges {
    segment_infos = apply_merge(
      &mut r,
      segment_infos,
      one_merge,
      &format!("_{}", seg_name_generator.fetch_add(1, Ordering::SeqCst)),
      &mut stats,
      fake_directory.clone(),
    )?;
  }

  assert_eq!(2, segment_infos.size());
  assert_eq!(55_000, segment_infos.info(0).unwrap().info.max_doc()?);
  assert_eq!(11_000, segment_infos.info(1).unwrap().info.max_doc()?);
  Ok(())
}

#[test]
fn test_one_small_middle_segment() -> Result<()> {
  let mut r = random();
  let merge_policy = LogMergePolicy::<LogDocMergePolicy>::log_doc();
  let mut stats = IOStats::default();
  let seg_name_generator = AtomicU64::new(0);
  let merge_context = MockMergeContext::new(|s: &SegmentCommitInfo<_>| Ok(s.get_del_count()));
  let fake_directory = Arc::new(FakeDirectory::new());
  let mut segment_infos = SegmentInfos::new(LATEST.major)?;

  for _ in 0..5 {
    segment_infos.add(make_segment_commit_info(
      &mut r,
      fake_directory.clone(),
      &format!("_{}", seg_name_generator.fetch_add(1, Ordering::SeqCst)),
      10_000,
      0,
      0.0,
      SOURCE_MERGE,
    )?)?;
  }

  segment_infos.add(make_segment_commit_info(
    &mut r,
    fake_directory.clone(),
    &format!("_{}", seg_name_generator.fetch_add(1, Ordering::SeqCst)),
    100,
    0,
    0.0,
    SOURCE_MERGE,
  )?)?;

  for _ in 0..5 {
    segment_infos.add(make_segment_commit_info(
      &mut r,
      fake_directory.clone(),
      &format!("_{}", seg_name_generator.fetch_add(1, Ordering::SeqCst)),
      10_000,
      0,
      0.0,
      SOURCE_MERGE,
    )?)?;
  }

  let spec_opt: Option<DefaultMergeSpecification<FakeDirectory>> =
    merge_policy.find_merges(MergeTrigger::Explicit, &segment_infos, None, &merge_context)?;
  assert!(spec_opt.is_some());
  let spec = spec_opt.unwrap();

  for one_merge in &spec.merges {
    segment_infos = apply_merge(
      &mut r,
      segment_infos,
      one_merge,
      &format!("_{}", seg_name_generator.fetch_add(1, Ordering::SeqCst)),
      &mut stats,
      fake_directory.clone(),
    )?;
  }

  assert_eq!(2, segment_infos.size());
  assert_eq!(90_100, segment_infos.info(0).unwrap().info.max_doc()?);
  assert_eq!(10_000, segment_infos.info(1).unwrap().info.max_doc()?);
  Ok(())
}

#[test]
fn test_many_small_middle_segment() -> Result<()> {
  let mut r = random();
  let merge_policy = LogMergePolicy::<LogDocMergePolicy>::log_doc();
  let mut stats = IOStats::default();
  let seg_name_generator = AtomicU64::new(0);
  let merge_context = MockMergeContext::new(|s: &SegmentCommitInfo<_>| Ok(s.get_del_count()));
  let fake_directory = Arc::new(FakeDirectory::new());
  let mut segment_infos = SegmentInfos::new(LATEST.major)?;

  segment_infos.add(make_segment_commit_info(
    &mut r,
    fake_directory.clone(),
    &format!("_{}", seg_name_generator.fetch_add(1, Ordering::SeqCst)),
    10_000,
    0,
    0.0,
    SOURCE_MERGE,
  )?)?;

  for _ in 0..9 {
    segment_infos.add(make_segment_commit_info(
      &mut r,
      fake_directory.clone(),
      &format!("_{}", seg_name_generator.fetch_add(1, Ordering::SeqCst)),
      100,
      0,
      0.0,
      SOURCE_MERGE,
    )?)?;
  }

  segment_infos.add(make_segment_commit_info(
    &mut r,
    fake_directory.clone(),
    &format!("_{}", seg_name_generator.fetch_add(1, Ordering::SeqCst)),
    10_000,
    0,
    0.0,
    SOURCE_MERGE,
  )?)?;

  let spec_opt: Option<DefaultMergeSpecification<FakeDirectory>> =
    merge_policy.find_merges(MergeTrigger::Explicit, &segment_infos, None, &merge_context)?;
  assert!(spec_opt.is_some());
  let spec = spec_opt.unwrap();

  for one_merge in &spec.merges {
    segment_infos = apply_merge(
      &mut r,
      segment_infos,
      one_merge,
      &format!("_{}", seg_name_generator.fetch_add(1, Ordering::SeqCst)),
      &mut stats,
      fake_directory.clone(),
    )?;
  }

  assert_eq!(2, segment_infos.size());
  assert_eq!(10_900, segment_infos.info(0).unwrap().info.max_doc()?);
  assert_eq!(10_000, segment_infos.info(1).unwrap().info.max_doc()?);
  Ok(())
}

#[test]
fn test_reject_unbalanced_merges() -> Result<()> {
  let mut r = random();
  let mut merge_policy = LogMergePolicy::<LogDocMergePolicy>::log_doc();
  merge_policy.set_min_merge_docs(10_000);
  let mut stats = IOStats::default();
  let seg_name_generator = AtomicU64::new(0);
  let merge_context = MockMergeContext::new(|s: &SegmentCommitInfo<_>| Ok(s.get_del_count()));
  let fake_directory = Arc::new(FakeDirectory::new());
  let mut segment_infos = SegmentInfos::new(LATEST.major)?;

  segment_infos.add(make_segment_commit_info(
    &mut r,
    fake_directory.clone(),
    &format!("_{}", seg_name_generator.fetch_add(1, Ordering::SeqCst)),
    100,
    0,
    0.0,
    SOURCE_MERGE,
  )?)?;

  for _ in 0..9 {
    segment_infos.add(make_segment_commit_info(
      &mut r,
      fake_directory.clone(),
      &format!("_{}", seg_name_generator.fetch_add(1, Ordering::SeqCst)),
      1,
      0,
      0.0,
      SOURCE_FLUSH,
    )?)?;
  }

  let spec_opt: Option<DefaultMergeSpecification<FakeDirectory>> =
    merge_policy.find_merges(MergeTrigger::Explicit, &segment_infos, None, &merge_context)?;
  assert!(spec_opt.is_none());

  segment_infos.add(make_segment_commit_info(
    &mut r,
    fake_directory.clone(),
    &format!("_{}", seg_name_generator.fetch_add(1, Ordering::SeqCst)),
    1,
    0,
    0.0,
    SOURCE_FLUSH,
  )?)?;

  let spec_opt: Option<DefaultMergeSpecification<FakeDirectory>> =
    merge_policy.find_merges(MergeTrigger::Explicit, &segment_infos, None, &merge_context)?;
  assert!(spec_opt.is_some());
  let spec = spec_opt.unwrap();

  for one_merge in &spec.merges {
    segment_infos = apply_merge(
      &mut r,
      segment_infos,
      one_merge,
      &format!("_{}", seg_name_generator.fetch_add(1, Ordering::SeqCst)),
      &mut stats,
      fake_directory.clone(),
    )?;
  }

  assert_eq!(2, segment_infos.size());
  assert_eq!(100, segment_infos.info(0).unwrap().info.max_doc()?);
  assert_eq!(10, segment_infos.info(1).unwrap().info.max_doc()?);
  Ok(())
}

#[test]
fn test_pack_large_segments() -> Result<()> {
  let mut r = random();
  let mut merge_policy = LogMergePolicy::<LogDocMergePolicy>::log_doc();
  merge_policy.set_max_merge_docs(10_000);
  let mut stats = IOStats::default();
  let seg_name_generator = AtomicU64::new(0);
  let merge_context = MockMergeContext::new(|s: &SegmentCommitInfo<_>| Ok(s.get_del_count()));
  let fake_directory = Arc::new(FakeDirectory::new());
  let mut segment_infos = SegmentInfos::new(LATEST.major)?;

  for _ in 0..10 {
    segment_infos.add(make_segment_commit_info(
      &mut r,
      fake_directory.clone(),
      &format!("_{}", seg_name_generator.fetch_add(1, Ordering::SeqCst)),
      3_000,
      0,
      0.0,
      SOURCE_MERGE,
    )?)?;
  }

  let spec_opt: Option<DefaultMergeSpecification<FakeDirectory>> =
    merge_policy.find_merges(MergeTrigger::Explicit, &segment_infos, None, &merge_context)?;
  assert!(spec_opt.is_some());
  let spec = spec_opt.unwrap();

  for one_merge in &spec.merges {
    segment_infos = apply_merge(
      &mut r,
      segment_infos,
      one_merge,
      &format!("_{}", seg_name_generator.fetch_add(1, Ordering::SeqCst)),
      &mut stats,
      fake_directory.clone(),
    )?;
  }

  assert_eq!(9_000, segment_infos.info(0).unwrap().info.max_doc()?);
  Ok(())
}

#[test]
fn test_ignore_large_segments() -> Result<()> {
  let mut r = random();
  let mut merge_policy = LogMergePolicy::<LogDocMergePolicy>::log_doc();
  merge_policy.set_max_merge_docs(10_000);
  let mut stats = IOStats::default();
  let seg_name_generator = AtomicU64::new(0);
  let merge_context = MockMergeContext::new(|s: &SegmentCommitInfo<_>| Ok(s.get_del_count()));
  let fake_directory = Arc::new(FakeDirectory::new());
  let mut segment_infos = SegmentInfos::new(LATEST.major)?;

  segment_infos.add(make_segment_commit_info(
    &mut r,
    fake_directory.clone(),
    &format!("_{}", seg_name_generator.fetch_add(1, Ordering::SeqCst)),
    11_000,
    0,
    0.0,
    SOURCE_MERGE,
  )?)?;

  for _ in 0..10 {
    segment_infos.add(make_segment_commit_info(
      &mut r,
      fake_directory.clone(),
      &format!("_{}", seg_name_generator.fetch_add(1, Ordering::SeqCst)),
      2_000,
      0,
      0.0,
      SOURCE_MERGE,
    )?)?;
  }

  let spec_opt: Option<DefaultMergeSpecification<FakeDirectory>> =
    merge_policy.find_merges(MergeTrigger::Explicit, &segment_infos, None, &merge_context)?;
  assert!(spec_opt.is_some());
  let spec = spec_opt.unwrap();

  for one_merge in &spec.merges {
    segment_infos = apply_merge(
      &mut r,
      segment_infos,
      one_merge,
      &format!("_{}", seg_name_generator.fetch_add(1, Ordering::SeqCst)),
      &mut stats,
      fake_directory.clone(),
    )?;
  }

  assert_eq!(11_000, segment_infos.info(0).unwrap().info.max_doc()?);
  assert_eq!(10_000, segment_infos.info(1).unwrap().info.max_doc()?);
  Ok(())
}

#[test]
fn test_full_flush_merges() -> Result<()> {
  let mut r = random();
  let case = TestLogMergePolicy;
  let mp = case.merge_policy(&mut r);

  let seg_name_generator = AtomicU64::new(0);
  let mut stats = IOStats::default();
  let merge_context = MockMergeContext::new(|s: &SegmentCommitInfo<_>| Ok(s.get_del_count()));
  let fake_directory = Arc::new(FakeDirectory::new());
  let mut segment_infos = SegmentInfos::new(LATEST.major)?;

  let num_segments_for_merging = match &mp {
    MergePolicyEnum::LogDoc(p) => p.get_merge_factor() + p.get_target_search_concurrency() as usize,
    MergePolicyEnum::LogBytesSize(p) => {
      p.get_merge_factor() + p.get_target_search_concurrency() as usize
    },
    _ => {
      return Err(LuceneError::illegal_state(
        "expected LogMergePolicy variant",
      ));
    },
  };

  for _ in 0..num_segments_for_merging {
    segment_infos.add(make_segment_commit_info(
      &mut r,
      fake_directory.clone(),
      &format!("_{}", seg_name_generator.fetch_add(1, Ordering::SeqCst)),
      1,
      0,
      f64::MIN_POSITIVE,
      SOURCE_FLUSH,
    )?)?;
  }

  let spec_opt = mp.find_full_flush_merges(
    MergeTrigger::FullFlush,
    &segment_infos,
    None,
    &merge_context,
  )?;
  assert!(spec_opt.is_some());
  let spec = spec_opt.unwrap();
  for merge in &spec.merges {
    segment_infos = apply_merge(
      &mut r,
      segment_infos,
      merge,
      &format!("_{}", seg_name_generator.fetch_add(1, Ordering::SeqCst)),
      &mut stats,
      fake_directory.clone(),
    )?;
  }
  assert!(segment_infos.size() < num_segments_for_merging);
  Ok(())
}
mod base_merge_policy_test_case_tests {
  use super::*;
  use crate::test::core::index::test_log_merge_policy::run_case;
  use crate::test_framework::core::index::base_merge_policy_test_case::FakeDirectory;
  use std::sync::Arc;

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
      // TODO IMPORTANT 默认的一亿篇文档速度很慢
      // case.test_simulate_append_only(random, &mp, Arc::new(FakeDirectory::new()))
      case.do_test_simulate_append_only(
        random,
        &mp,
        Arc::new(FakeDirectory::new()),
        50_000_000,
        10_000,
      )
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
    run_case(|case, random| {
      let mp = case.merge_policy(random);
      case.test_no_pathological_merges(random, &mp, Arc::new(FakeDirectory::new()))
    })
  }
}
