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
use crate::core::document::document::Document;
use crate::core::document::field::Store;
use crate::core::document::field_type::FieldType;
use crate::core::document::stored_field::StoredField;
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::directory_reader;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::{IndexWriter, SOURCE_FLUSH, SOURCE_MERGE};
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::merge_policy::{MergePolicy, MergePolicyEnum, MergeSpecification};
use crate::core::index::merge_trigger::MergeTrigger;
use crate::core::index::segment_commit_info::SegmentCommitInfo;
use crate::core::index::segment_infos::SegmentInfos;
use crate::core::index::serial_merge_scheduler::SerialMergeScheduler;
use crate::core::index::term::Term;
use crate::core::index::terms::Terms;
use crate::core::index::tiered_merge_policy::TieredMergePolicy;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::store::directory::Directory;
use crate::core::util::LATEST;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::index::base_merge_policy_test_case::{
  BaseMergePolicyTestCase, FakeDirectory, IOStats, MockMergeContext, apply_deletes, apply_merge,
  make_segment_commit_info,
};
use crate::test_framework::core::util::lucene_test_case::{
  at_least, is_night_mode, new_directory_shared, new_field, new_index_writer_config_with_analyzer,
  new_text_field, new_tiered_merge_policy, random, random_multiplier,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::prelude::StdRng;
use rand::{Rng, RngExt};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

pub struct TestTieredMergePolicy;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocCountAndSizeInBytes {
  pub doc_count: i32,
  pub size_in_bytes: i64,
}
fn run_case<F>(f: F) -> Result<()>
where
  F: FnOnce(&TestTieredMergePolicy, &mut StdRng) -> Result<()>,
{
  let mut random = random();
  let case = TestTieredMergePolicy;
  f(&case, &mut random)
}
mod base_merge_policy_test_case_tests {
  use super::*;
  use crate::test::core::index::test_tiered_merge_policy::run_case;

  #[test]
  fn test_rare_vectors() -> Result<()> {
    run_case(|case, random| case.test_force_merge_not_needed(random))
  }
  #[test]
  fn test_find_forced_deletes_merges() -> Result<()> {
    run_case(|case, random| case.test_find_forced_deletes_merges(random))
  }
  #[test]
  fn test_simulate_append_only() -> Result<()> {
    run_case(|case, random| {
      let mut merge_policy = case.merge_policy::<FakeDirectory, _>(random);
      // Avoid low values of the max merged segment size which prevent this merge policy from
      // scaling well.
      merge_policy.set_max_merged_segment_mb(TestUtil::next_int(random, 1024, 10 * 1024) as f64)?;
      case.do_test_simulate_append_only(
        random,
        &merge_policy,
        Arc::new(FakeDirectory::new()),
        100_000_000,
        10_000,
      )
    })
  }
  #[test]
  fn test_simulate_updates() -> Result<()> {
    run_case(|case, random| {
      let mut merge_policy = case.merge_policy::<FakeDirectory, _>(random);
      // Avoid low values of the max merged segment size which prevent this merge policy from
      // scaling well.
      merge_policy.set_max_merged_segment_mb(TestUtil::next_int(random, 1024, 10 * 1024) as f64)?;
      let num_docs = if is_night_mode() {
        at_least(random, 10_000_000)
      } else {
        at_least(random, 1_000_000)
      };
      case.do_test_simulate_updates(
        random,
        &merge_policy,
        Arc::new(FakeDirectory::new()),
        num_docs,
        2500,
      )
    })
  }
  #[test]
  fn test_no_pathological_merges() -> Result<()> {
    run_case(|case, random| {
      let mp = case.merge_policy::<FakeDirectory, _>(random);
      case.test_no_pathological_merges(random, &mp, Arc::new(FakeDirectory::new()))
    })
  }
}
impl BaseMergePolicyTestCase for TestTieredMergePolicy {
  type MergePolicy<D>
    = TieredMergePolicy
  where
    D: Directory;

  fn merge_policy<D, R>(&self, random: &mut R) -> Self::MergePolicy<D>
  where
    D: Directory,
    R: Rng + ?Sized,
  {
    new_tiered_merge_policy(random).expect("randomized TieredMergePolicy settings must be valid")
  }

  fn assert_segment_infos<D>(tmp: &Self::MergePolicy<D>, infos: &SegmentInfos<D>) -> Result<()>
  where
    D: Directory,
  {
    let max_merged_segment_bytes = (tmp.get_max_merged_segment_mb() * 1024.0 * 1024.0) as i64;

    let mut min_segment_bytes = i64::MAX;
    let mut total_del_count = 0i32;
    let mut total_max_doc = 0i32;
    let mut total_bytes = 0i64;
    let mut segment_sizes = Vec::new();

    for i in 0..infos.size() {
      let sci = infos.info(i).unwrap();
      total_del_count += sci.get_del_count();
      total_max_doc += sci.info.max_doc()?;
      let byte_size = sci.size_in_bytes()?;
      let live_ratio = 1.0 - (sci.get_del_count() as f64) / (sci.info.max_doc()? as f64);
      let weighted_byte_size = (live_ratio * byte_size as f64) as i64;
      total_bytes += weighted_byte_size;
      segment_sizes.push(DocCountAndSizeInBytes {
        doc_count: sci.info.max_doc()? - sci.get_del_count(),
        size_in_bytes: weighted_byte_size,
      });
      min_segment_bytes = std::cmp::min(min_segment_bytes, weighted_byte_size);
    }

    segment_sizes.sort_by_key(|v| v.size_in_bytes);

    let del_percentage = 100.0 * (total_del_count as f64) / (total_max_doc as f64);
    assert!(
      del_percentage <= tmp.get_deletes_pct_allowed(),
      "Percentage of deleted docs {} is larger than the target: {}",
      del_percentage,
      tmp.get_deletes_pct_allowed()
    );

    let mut level_size_bytes = std::cmp::max(
      min_segment_bytes,
      (tmp.get_floor_segment_mb() * 1024.0 * 1024.0) as i64,
    );
    let mut bytes_left = total_bytes;
    let mut allowed_seg_count = 0.0_f64;

    let mut biggest_segments = &segment_sizes[..];
    if biggest_segments.len() as i32 > tmp.get_target_search_concurrency() - 1 {
      biggest_segments = &biggest_segments
        [(biggest_segments.len() as i32 - tmp.get_target_search_concurrency() + 1) as usize..];
    }

    for size in biggest_segments {
      bytes_left -= size.size_in_bytes;
      allowed_seg_count += 1.0;
    }

    let mut too_big_count = 0i32;
    for size in &segment_sizes {
      if size.size_in_bytes >= max_merged_segment_bytes / 2 {
        too_big_count += 1;
      }
    }

    let merge_factor = std::cmp::min(
      tmp.get_segments_per_tier() as i32,
      tmp.get_max_merge_at_once(),
    );
    loop {
      let seg_count_level = bytes_left as f64 / level_size_bytes as f64;
      if seg_count_level <= tmp.get_segments_per_tier()
        || level_size_bytes >= max_merged_segment_bytes / 2
      {
        allowed_seg_count += seg_count_level.ceil();
        break;
      }
      allowed_seg_count += tmp.get_segments_per_tier();
      bytes_left -= (tmp.get_segments_per_tier() as i64) * level_size_bytes;
      level_size_bytes = std::cmp::min(
        level_size_bytes * merge_factor as i64,
        max_merged_segment_bytes / 2,
      );
    }

    allowed_seg_count = allowed_seg_count.max(too_big_count as f64 + tmp.get_segments_per_tier());
    allowed_seg_count = allowed_seg_count.max(tmp.get_target_search_concurrency() as f64);

    let max_docs_per_segment = tmp.get_max_allowed_docs(infos.total_max_doc()?, total_del_count);
    let mut has_legal_merges = false;

    for i in 0..segment_sizes.len().saturating_sub(1) {
      let size1 = &segment_sizes[i];
      let size2 = &segment_sizes[i + 1];
      let merged_segment_size_in_bytes = size1.size_in_bytes + size2.size_in_bytes;
      let merged_segment_doc_count = size1.doc_count + size2.doc_count;

      if merged_segment_size_in_bytes <= max_merged_segment_bytes
        && (size2.size_in_bytes as f64) * 1.5 <= merged_segment_size_in_bytes as f64
        && merged_segment_doc_count <= max_docs_per_segment
      {
        has_legal_merges = true;
        break;
      }
    }

    let num_segments = infos.size();

    assert!(
      num_segments as f64 <= allowed_seg_count || !has_legal_merges,
      "mergeFactor={} minSegmentBytes={:?} maxMergedSegmentBytes={} segmentsPerTier={} maxMergeAtOnce={} numSegments={} allowed={} totalBytes={} delPercentage={} deletesPctAllowed={} targetNumSegments={}",
      merge_factor,
      min_segment_bytes,
      max_merged_segment_bytes,
      tmp.get_segments_per_tier(),
      tmp.get_max_merge_at_once(),
      num_segments,
      allowed_seg_count,
      total_bytes,
      del_percentage,
      tmp.get_deletes_pct_allowed(),
      tmp.get_target_search_concurrency(),
    );

    Ok(())
  }

  fn assert_merge<D, CR>(
    tmp: &Self::MergePolicy<D>,
    merges: &MergeSpecification<D, CR>,
  ) -> Result<()>
  where
    D: Directory,
    CR: CodecReader,
  {
    for merge in &merges.merges {
      assert!(merge.stat.segments.len() <= tmp.get_max_merge_at_once() as usize);
    }
    Ok(())
  }
}

#[test]
fn test_force_merge_deletes() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;

  let mut tmp = new_tiered_merge_policy(&mut random)?;

  tmp.set_max_merge_at_once(100)?;
  tmp.set_segments_per_tier(100.0)?;
  tmp.set_deletes_pct_allowed(50.0)?;
  tmp.set_force_merge_deletes_pct_allowed(30.0)?;
  conf.set_merge_policy(tmp);
  conf.set_max_buffered_docs(4);

  let w = IndexWriter::new(dir.clone(), conf)?;

  let mut field_to_type = HashMap::new();

  for i in 0..80 {
    let mut doc = Document::new();
    let value = format!("aaa {}", i % 4);
    doc.add(new_text_field(
      &mut random,
      "content",
      &value,
      Store::No,
      &mut field_to_type,
    )?);
    w.add_document(doc)?;
  }

  assert_eq!(80, w.get_doc_stats()?.max_doc);
  assert_eq!(80, w.get_doc_stats()?.num_docs);

  if cfg!(feature = "test_log_verbose") {
    println!("\nTEST: delete docs");
  }

  w.delete_documents_with_terms(vec![Term::from_text("content", "0")])?;
  w.force_merge_deletes()?;

  assert_eq!(80, w.get_doc_stats()?.max_doc);
  assert_eq!(60, w.get_doc_stats()?.num_docs);

  if cfg!(feature = "test_log_verbose") {
    println!("\nTEST: forceMergeDeletes2");
  }

  let mp = match w.get_config_mut().get_merge_policy_mut() {
    MergePolicyEnum::Tiered(t) => t,
    _ => unreachable!(""),
  };
  mp.set_force_merge_deletes_pct_allowed(10.0)?;

  w.force_merge_deletes()?;

  assert_eq!(60, w.get_doc_stats()?.max_doc);
  assert_eq!(60, w.get_doc_stats()?.num_docs);

  w.close()?;
  Ok(())
}
#[test]
fn test_partial_merge() -> Result<()> {
  let mut random = random();
  let num = at_least(&mut random, 10);

  for iter in 0..num {
    if cfg!(feature = "test_log_verbose") {
      println!("TEST: iter={}", iter);
    }

    let dir = new_directory_shared(&mut random)?;

    let analyzer = MockAnalyzer::new(&mut random);
    let mut conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;

    conf.set_merge_scheduler(SerialMergeScheduler::new());

    let mut tmp = new_tiered_merge_policy(&mut random)?;
    tmp.set_max_merge_at_once(3)?;
    tmp.set_segments_per_tier(6.0)?;

    let max_merged_segment_mb = tmp.get_max_merged_segment_mb();
    let floor_segment_mb = tmp.get_floor_segment_mb();
    conf.set_merge_policy(tmp);
    conf.set_max_buffered_docs(2);

    let w = IndexWriter::new(dir.clone(), conf)?;

    let mut field_to_type = HashMap::new();

    let mut max_count = 0;
    let num_docs = TestUtil::next_int(&mut random, 20, 100);

    for i in 0..num_docs {
      let mut doc = Document::new();
      let value = format!("aaa {}", i % 4);
      doc.add(new_text_field(
        &mut random,
        "content",
        &value,
        Store::No,
        &mut field_to_type,
      )?);

      w.add_document(doc)?;

      let count = w.get_segment_count();
      max_count = std::cmp::max(count, max_count);

      assert!(
        count + 3 >= max_count,
        "count={} maxCount={}",
        count,
        max_count
      );
    }

    w.flush_with_apply_merge_deletes(true, true)?;

    let segment_count = w.get_segment_count();
    let target_count = TestUtil::next_int(&mut random, 1, segment_count as i32);

    if cfg!(feature = "test_log_verbose") {
      println!(
        "TEST: merge to {} segs (current count={})",
        target_count, segment_count
      );
    }

    w.force_merge(target_count)?;

    let max_segment_size = f64::max(max_merged_segment_mb, floor_segment_mb);

    let max125_pct = (max_segment_size * 1024.0 * 1024.0 * 1.25) as i64;

    if target_count == 1 {
      assert_eq!(target_count as usize, w.get_segment_count(),);
    } else {
      let infos = w.clone_segment_infos()?;

      for i in 0..infos.size() {
        let info = infos.info(i).unwrap();
        assert!(
          max125_pct >= info.size_in_bytes()?,
          "No segment should be more than 125% of max segment size"
        );
      }
    }

    w.close()?;
  }

  Ok(())
}
#[test]
fn test_force_merge_deletes_max_seg_size() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;

  let mut tmp = TieredMergePolicy::new();
  tmp.set_max_merged_segment_mb(0.01)?;
  tmp.set_force_merge_deletes_pct_allowed(0.0)?;
  conf.set_merge_policy(tmp);

  let w = IndexWriter::new(dir.clone(), conf)?;

  let mut field_to_type = HashMap::new();

  let num_docs = at_least(&mut random, 200);

  for i in 0..num_docs {
    let mut doc = Document::new();
    doc.add(new_field(
      &mut random,
      "id",
      i.to_string(),
      &FieldType::from_ref(&*crate::core::document::string_field::TYPE_NOT_STORED)?,
      &mut field_to_type,
    )?);
    doc.add(new_text_field(
      &mut random,
      "content",
      format!("aaa {}", i),
      Store::No,
      &mut field_to_type,
    )?);
    w.add_document(doc)?;
  }

  w.force_merge(1)?;

  let reader = directory_reader::open_from_writer(&w)?;
  assert_eq!(num_docs, reader.max_doc()?);
  assert_eq!(num_docs, reader.num_docs()?);
  reader.close()?;

  if cfg!(feature = "test_log_verbose") {
    println!("\nTEST: delete doc");
  }

  let term_val = (42 + 17).to_string();
  w.delete_documents_with_terms(vec![Term::from_text("id", &term_val)])?;

  let reader = directory_reader::open_from_writer(&w)?;
  assert_eq!(num_docs, reader.max_doc()?);
  assert_eq!(num_docs - 1, reader.num_docs()?);
  reader.close()?;

  w.force_merge_deletes()?;

  let reader = directory_reader::open_from_writer(&w)?;
  assert_eq!(num_docs - 1, reader.max_doc()?);
  assert_eq!(num_docs - 1, reader.num_docs()?);
  reader.close()?;

  w.close()?;
  Ok(())
}
#[test]
fn test_forced_merges_respect_seg_size() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  let mut tmp = TieredMergePolicy::new();

  let mb_size = 0.004;
  let max_seg_bytes = (1024.0 * 1024.0) as i64;
  tmp.set_max_merged_segment_mb(mb_size)?;
  conf.set_max_buffered_docs(100);
  conf.set_merge_policy(tmp);
  conf.set_merge_scheduler(SerialMergeScheduler::new());

  let w = IndexWriter::new(dir.clone(), conf)?;

  let mut field_to_type = HashMap::new();

  let num_docs = at_least(&mut random, 2400);
  for i in 0..num_docs {
    let mut doc = Document::new();
    doc.add(new_field(
      &mut random,
      "id",
      i.to_string(),
      &FieldType::from_ref(&*crate::core::document::string_field::TYPE_NOT_STORED)?,
      &mut field_to_type,
    )?);
    doc.add(new_text_field(
      &mut random,
      "content",
      format!("aaa {}", i),
      Store::No,
      &mut field_to_type,
    )?);
    w.add_document(doc)?;
  }

  w.commit()?;

  let mut seg_names_before = get_segment_names(&w)?;
  w.force_merge_deletes()?;
  check_segments_in_expectations(&w, &seg_names_before, false)?;
  w.force_merge(i32::MAX)?;
  check_segments_in_expectations(&w, &seg_names_before, true)?;
  check_segment_size_not_exceeded(&w.clone_segment_infos()?, max_seg_bytes)?;

  let pct = TestUtil::next_int(&mut random, 0, 4) + 12;
  let mut remaining_docs = num_docs - delete_pct_docs_from_each_seg(&w, pct, true)?;
  w.force_merge_deletes()?;
  w.commit()?;
  check_segment_size_not_exceeded(&w.clone_segment_infos()?, max_seg_bytes)?;
  assert!(!w.has_deletions()?);

  seg_names_before = get_segment_names(&w)?;
  let pct = TestUtil::next_int(&mut random, 0, 3) + 3;
  let deleted_this_pass = delete_pct_docs_from_each_seg(&w, pct, false)?;
  w.force_merge_deletes()?;
  remaining_docs -= deleted_this_pass;
  check_segments_in_expectations(&w, &seg_names_before, false)?;
  assert_eq!(remaining_docs, w.get_doc_stats()?.num_docs);
  assert!(w.get_doc_stats()?.num_docs < w.get_doc_stats()?.max_doc);

  w.force_merge(i32::MAX)?;
  check_segment_size_not_exceeded(&w.clone_segment_infos()?, max_seg_bytes)?;

  w.force_merge(1)?;
  assert_eq!(1, w.get_segment_count());
  assert_eq!(w.get_doc_stats()?.num_docs, w.get_doc_stats()?.max_doc);
  assert_eq!(remaining_docs, w.get_doc_stats()?.num_docs);

  seg_names_before = get_segment_names(&w)?;
  let pct = TestUtil::next_int(&mut random, 0, 4) + 1;
  remaining_docs -= delete_pct_docs_from_each_seg(&w, pct, false)?;
  w.force_merge_deletes()?;
  check_segments_in_expectations(&w, &seg_names_before, false)?;
  assert_eq!(1, w.get_segment_count());
  assert!(w.get_doc_stats()?.num_docs < w.get_doc_stats()?.max_doc);

  w.force_merge(1)?;

  let pct = TestUtil::next_int(&mut random, 0, 4) + 20;
  remaining_docs -= delete_pct_docs_from_each_seg(&w, pct, true)?;
  w.force_merge_deletes()?;

  assert_eq!(1, w.get_segment_count());
  assert_eq!(w.get_doc_stats()?.num_docs, w.get_doc_stats()?.max_doc);

  assert!(w.get_doc_stats()?.num_docs > 1_000);

  let pct = (w.get_doc_stats()?.num_docs * 60) / 100;
  let deleted_this_pass = delete_pct_docs_from_each_seg(&w, pct, true)?;
  remaining_docs -= deleted_this_pass;

  for i in 0..50 {
    let mut doc = Document::new();
    doc.add(new_field(
      &mut random,
      "id",
      (i + num_docs).to_string(),
      &FieldType::from_ref(&*crate::core::document::string_field::TYPE_NOT_STORED)?,
      &mut field_to_type,
    )?);
    doc.add(new_text_field(
      &mut random,
      "content",
      format!("aaa {}", i),
      Store::No,
      &mut field_to_type,
    )?);
    w.add_document(doc)?;
  }

  w.commit()?;

  let infos = w.clone_segment_infos()?;
  assert_eq!(2, infos.size());

  let info0 = infos.info(0).unwrap();
  let info1 = infos.info(1).unwrap();
  let large_seg_doc_count = std::cmp::max(info0.info.max_doc()?, info1.info.max_doc()?);
  let small_seg_doc_count = std::cmp::min(info0.info.max_doc()?, info1.info.max_doc()?);

  assert_eq!(large_seg_doc_count, remaining_docs);
  assert_eq!(small_seg_doc_count, 50);

  w.close()?;
  Ok(())
}

fn post_merges_segment_count<D, CR>(
  starting_segment_count: i32,
  spec: &MergeSpecification<D, CR>,
) -> i32
where
  D: Directory,
  CR: CodecReader,
{
  let mut count = starting_segment_count;

  for merge in &spec.merges {
    count -= merge.stat.segments.len() as i32;
  }

  count += spec.merges.len() as i32;

  count
}
fn assert_max_merged_size<D, CR>(
  specification: &MergeSpecification<D, CR>,
  max_merged_segment_size_mb: f64,
  index_total_size_in_mb: f64,
  max_merged_segment_count: i32,
  infos: &SegmentInfos<D>,
) -> Result<()>
where
  D: Directory,
  CR: CodecReader,
{
  let max_mb_per_segment = index_total_size_in_mb / (max_merged_segment_count as f64);

  for merge in &specification.merges {
    let mut merge_total_size_in_bytes = 0i64;
    for segment_id in &merge.stat.segments {
      let segment = infos.index_of(segment_id).unwrap();
      merge_total_size_in_bytes += segment.size_in_bytes()?;
    }

    let limit_bytes =
      (1024.0 * 1024.0 * f64::max(max_mb_per_segment, max_merged_segment_size_mb) * 1.5) as i64;

    assert!(
      merge_total_size_in_bytes < limit_bytes,
      "mergeTotalSizeInBytes={} limitBytes={} maxMergedSegmentSizeMb={}",
      merge_total_size_in_bytes,
      limit_bytes,
      max_merged_segment_size_mb
    );
  }

  Ok(())
}

#[test]
fn test_forced_merges_use_least_number_of_merges() -> Result<()> {
  let mut random = random();
  let fake_directory = Arc::new(FakeDirectory::new());

  let mut tmp = TieredMergePolicy::new();
  let mut one_segment_size_mb = 1.0_f64;
  let max_merged_segment_size_mb = 10.0 * one_segment_size_mb;
  tmp.set_max_merged_segment_mb(max_merged_segment_size_mb)?;

  if cfg!(feature = "test_log_verbose") {
    println!(
      "TEST: maxMergedSegmentSizeMB={:.2}",
      max_merged_segment_size_mb
    );
  }

  let mut infos = SegmentInfos::new(LATEST.major)?;
  let segment_count = 30;
  for j in 0..segment_count {
    infos.add(make_segment_commit_info(
      &mut random,
      fake_directory.clone(),
      &format!("_{}", j),
      1000,
      0,
      one_segment_size_mb,
      SOURCE_MERGE,
    )?)?;
  }

  let mut index_total_size_mb = (segment_count as f64) * one_segment_size_mb;

  let max_segment_count_after_force_merge = random.random_range(0..10) + 3;
  if cfg!(feature = "test_log_verbose") {
    println!(
      "TEST: maxSegmentCountAfterForceMerge={}",
      max_segment_count_after_force_merge
    );
  }

  let specification = match tmp.find_forced_merges(
    &infos,
    max_segment_count_after_force_merge as usize,
    &segments_to_merge(&infos),
    None,
    &MockMergeContext::new(|s: &SegmentCommitInfo<_>| Ok(s.get_del_count())),
  )? {
    Some(spec) => spec,
    None => {
      return Err(LuceneError::illegal_state(
        "find_forced_merges returned None",
      ));
    },
  };

  assert_max_merged_size(
    &specification,
    max_merged_segment_size_mb,
    index_total_size_mb,
    max_segment_count_after_force_merge,
    &infos,
  )?;

  assert_eq!(
    max_segment_count_after_force_merge,
    post_merges_segment_count(infos.size() as i32, &specification)
  );

  infos = SegmentInfos::new(LATEST.major)?;
  let many_segments_count = at_least(&mut random, 100);
  if cfg!(feature = "test_log_verbose") {
    println!("TEST: manySegmentsCount={}", many_segments_count);
  }

  one_segment_size_mb = 0.1_f64;
  for j in 0..many_segments_count {
    infos.add(make_segment_commit_info(
      &mut random,
      fake_directory.clone(),
      &format!("_{}", j),
      1000,
      0,
      one_segment_size_mb,
      SOURCE_MERGE,
    )?)?;
  }

  index_total_size_mb = (many_segments_count as f64) * one_segment_size_mb;

  let specification = match tmp.find_forced_merges(
    &infos,
    max_segment_count_after_force_merge as usize,
    &segments_to_merge(&infos),
    None,
    &MockMergeContext::new(|s: &SegmentCommitInfo<_>| Ok(s.get_del_count())),
  )? {
    Some(spec) => spec,
    None => {
      return Err(LuceneError::illegal_state(
        "find_forced_merges returned None",
      ));
    },
  };

  assert_max_merged_size(
    &specification,
    max_merged_segment_size_mb,
    index_total_size_mb,
    max_segment_count_after_force_merge,
    &infos,
  )?;

  assert!(
    post_merges_segment_count(infos.size() as i32, &specification)
      >= max_segment_count_after_force_merge
  );

  Ok(())
}
#[test]
fn test_forced_merge_with_pending() -> Result<()> {
  let mut random = random();
  let fake_directory = Arc::new(FakeDirectory::new());

  let mut tmp = TieredMergePolicy::new();
  let max_segment_size = 10.0_f64;
  tmp.set_max_merged_segment_mb(max_segment_size)?;

  let mut infos = SegmentInfos::new(LATEST.major)?;
  for j in 0..30 {
    infos.add(make_segment_commit_info(
      &mut random,
      fake_directory.clone(),
      &format!("_{}", j),
      1000,
      0,
      1.0_f64,
      SOURCE_MERGE,
    )?)?;
  }

  let mut merge_context =
    MockMergeContext::new(|s: &SegmentCommitInfo<_>| Ok(s.get_del_count()));
  let merging = infos.info(0).unwrap();
  merge_context.set_merging_segments(HashSet::from([merging.info.get_id_key().to_string()]));

  let expected_count = random.random_range(0..10) + 3;

  let specification = tmp.find_forced_merges(
    &infos,
    expected_count as usize,
    &segments_to_merge(&infos),
    None,
    &merge_context,
  )?;

  assert!(specification.is_none());

  Ok(())
}
fn segments_to_merge<D>(infos: &SegmentInfos<D>) -> HashMap<String, Option<bool>>
where
  D: Directory,
{
  let mut segments_to_merge = HashMap::new();
  for i in 0..infos.size() {
    let info = infos.info(i).unwrap();
    segments_to_merge.insert(info.info.get_id_key().to_string(), Some(true));
  }
  segments_to_merge
}
// Having a segment with very few documents in it can happen because of the random nature of the
// docs added to the index. For instance, let's say it just happens that the last segment has 3
// docs in it.
// It can easily be merged with a close-to-max sized segment during a forceMerge and still respect
// the max segment
// size.
//
// If the above is possible, the "twoMayHaveBeenMerged" will be true and we allow for a little
// slop, checking that
// exactly two segments are gone from the old list and exactly one is in the new list. Otherwise,
// the lists must match
// exactly.
//
// So forceMerge may not be a no-op, allow for that. There are two possibilities in forceMerge
// only:
// > there were no small segments, in which case the two lists will be identical
// > two segments in the original list are replaced by one segment in the final list.
//
// finally, there are some cases of forceMerge where the expectation is that there be exactly no
// differences.
// this should be called after forceDeletesMerges with the boolean always false,
// Depending on the state, forceMerge may call with the boolean true or false.
fn check_segments_in_expectations<D>(
  w: &IndexWriter<D>,
  seg_names_before: &[String],
  two_may_have_been_merged: bool,
) -> Result<()>
where
  D: Directory,
{
  let seg_names_after = get_segment_names(w)?;

  if !two_may_have_been_merged || seg_names_after.len() == seg_names_before.len() {
    if seg_names_after.len() != seg_names_before.len() {
      panic!(
        "Segment lists different sizes!: {:?} After list: {:?}",
        seg_names_before, seg_names_after
      );
    }

    let before_set: HashSet<_> = seg_names_before.iter().collect();
    let after_set: HashSet<_> = seg_names_after.iter().collect();
    if !after_set.is_superset(&before_set) {
      panic!(
        "Segment lists should be identical: {:?} After list: {:?}",
        seg_names_before, seg_names_after
      );
    }
    return Ok(());
  }

  if seg_names_after.len() != seg_names_before.len() - 1 {
    panic!(
      "forceMerge didn't merge a small and large segment into one segment as expected: {:?} After list: {:?}",
      seg_names_before, seg_names_after
    );
  }

  let before_set: HashSet<_> = seg_names_before.iter().cloned().collect();
  let after_set: HashSet<_> = seg_names_after.iter().cloned().collect();

  let test_before: Vec<_> = before_set.difference(&after_set).cloned().collect();
  let test_after: Vec<_> = after_set.difference(&before_set).cloned().collect();

  if test_before.len() != 2 || test_after.len() != 1 {
    panic!(
      "Expected two unique 'before' segments and one unique 'after' segment: {:?} After list: {:?}",
      seg_names_before, seg_names_after
    );
  }

  Ok(())
}
fn get_segment_names<D>(w: &IndexWriter<D>) -> Result<Vec<String>>
where
  D: Directory,
{
  let infos = w.clone_segment_infos()?;
  let mut names = Vec::with_capacity(infos.size());
  for i in 0..infos.size() {
    let info = infos.info(i).unwrap();
    names.push(info.info.name.clone());
  }
  Ok(names)
}

fn delete_pct_docs_from_each_seg<D>(
  w: &Arc<IndexWriter<D>>,
  pct: i32,
  round_up: bool,
) -> Result<i32>
where
  D: Directory + 'static,
{
  let reader = directory_reader::open_from_writer(w)?;
  let reader = reader.get_context()?;
  let mut to_delete = Vec::new();
  for ctx in reader.leaves()? {
    to_delete.extend(get_rand_terms(ctx, pct, round_up)?);
  }

  w.delete_documents_with_terms(to_delete.clone())?;
  w.commit()?;
  Ok(to_delete.len() as i32)
}

fn get_rand_terms<LR>(ctx: &LeafReaderContext<LR>, pct: i32, round_up: bool) -> Result<Vec<Term>>
where
  LR: LeafReader,
{
  assert!(
    !ctx.reader().has_deletions()?,
    "This method assumes no deleted documents"
  );

  let mut ret = Vec::with_capacity(100);

  let num_docs = ctx.reader().num_docs()? as f64;
  let tmp = (num_docs * (pct as f64)) / 100.0;

  if tmp <= 1.0 {
    return Ok(ret);
  }

  let mod_ = (num_docs / tmp) as i32;
  if mod_ == 0 {
    return Ok(ret);
  }

  let terms = match ctx.reader().terms("id")? {
    Some(v) => v,
    None => return Ok(ret),
  };
  let mut iter = terms.iterator()?;
  let mut counter = 0i32;

  let mut lim = (num_docs * (pct as f64) / 100.0) as i32;
  if round_up {
    lim += 1;
  }

  while ret.len() < lim as usize {
    let br = iter.next()?;
    match br {
      Some(br) => {
        if (counter % mod_) == 0 {
          ret.push(Term::new("id", br.into_owned()));
        }
        counter += 1;
      },
      None => break,
    }
  }

  Ok(ret)
}

fn check_segment_size_not_exceeded<D>(infos: &SegmentInfos<D>, max_seg_bytes: i64) -> Result<()>
where
  D: Directory,
{
  for i in 0..infos.size() {
    let info = infos.info(i).unwrap();
    assert!(
      info.size_in_bytes()? <= max_seg_bytes,
      "Found an unexpectedly large segment: {}",
      info
    );
  }
  Ok(())
}
const EPSILON: f64 = 1e-14;
#[test]
fn test_setters() -> Result<()> {
  let mut tmp = TieredMergePolicy::new();

  tmp.set_max_merged_segment_mb(0.5)?;
  assert!((tmp.get_max_merged_segment_mb() - 0.5).abs() < EPSILON);

  tmp.set_max_merged_segment_mb(f64::INFINITY)?;
  assert!(
    (tmp.get_max_merged_segment_mb() - (i64::MAX as f64 / 1024.0 / 1024.0)).abs()
      < EPSILON * i64::MAX as f64
  );

  tmp.set_max_merged_segment_mb(i64::MAX as f64 / 1024.0 / 1024.0)?;
  assert!(
    (tmp.get_max_merged_segment_mb() - (i64::MAX as f64 / 1024.0 / 1024.0)).abs()
      < EPSILON * i64::MAX as f64
  );

  let err = tmp.set_max_merged_segment_mb(-2.0);
  assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));

  tmp.set_floor_segment_mb(2.0)?;
  assert!((tmp.get_floor_segment_mb() - 2.0).abs() < EPSILON);

  tmp.set_floor_segment_mb(f64::INFINITY)?;
  assert!(
    (tmp.get_floor_segment_mb() - (i64::MAX as f64 / 1024.0 / 1024.0)).abs()
      < EPSILON * i64::MAX as f64
  );

  tmp.set_floor_segment_mb(i64::MAX as f64 / 1024.0 / 1024.0)?;
  assert!(
    (tmp.get_floor_segment_mb() - (i64::MAX as f64 / 1024.0 / 1024.0)).abs()
      < EPSILON * i64::MAX as f64
  );

  let err = tmp.set_floor_segment_mb(-2.0);
  assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));

  MergePolicy::<FakeDirectory>::get_base_mut(&mut tmp).set_max_cfs_segment_size_mb(2.0)?;
  assert!(
    (MergePolicy::<FakeDirectory>::get_base(&tmp).get_max_cfs_segment_size_mb() - 2.0).abs()
      < EPSILON
  );

  MergePolicy::<FakeDirectory>::get_base_mut(&mut tmp)
    .set_max_cfs_segment_size_mb(f64::INFINITY)?;
  assert!(
    (MergePolicy::<FakeDirectory>::get_base(&tmp).get_max_cfs_segment_size_mb()
      - (i64::MAX as f64 / 1024.0 / 1024.0))
      .abs()
      < EPSILON * i64::MAX as f64
  );

  MergePolicy::<FakeDirectory>::get_base_mut(&mut tmp)
    .set_max_cfs_segment_size_mb(i64::MAX as f64 / 1024.0 / 1024.0)?;
  assert!(
    (MergePolicy::<FakeDirectory>::get_base(&tmp).get_max_cfs_segment_size_mb()
      - (i64::MAX as f64 / 1024.0 / 1024.0))
      .abs()
      < EPSILON * i64::MAX as f64
  );

  let err = MergePolicy::<FakeDirectory>::get_base_mut(&mut tmp).set_max_cfs_segment_size_mb(-2.0);
  assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));

  Ok(())
}
#[test]
fn test_unbalanced_merge_selection() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  iwc.set_codec(TestUtil::get_default_codec());

  let tmp = match iwc.get_merge_policy_mut() {
    MergePolicyEnum::Tiered(t) => t,
    _ => unreachable!(),
  };
  tmp.set_floor_segment_mb(0.00001)?;

  iwc.set_merge_scheduler(SerialMergeScheduler::new());
  iwc.set_max_buffered_docs(100);
  iwc.set_ram_buffer_size_mb(-1.0);

  let w = IndexWriter::new(dir.clone(), iwc)?;

  for _ in 0..15000 * random_multiplier() {
    let mut doc = Document::new();
    let mut id_bytes = vec![0u8; 128];
    random.fill(&mut id_bytes[..]);
    doc.add(StoredField::from_binary("id", id_bytes)?);
    w.add_document(doc)?;
  }

  let r = (directory_reader::open_from_writer(&w)?).get_context()?;

  for ctx in r.leaves()? {
    let num_docs = ctx.reader().num_docs()?;
    assert!(
      num_docs == 100 || num_docs == 1000 || num_docs == 10000,
      "got numDocs={}",
      num_docs
    );
  }
  w.close()?;
  Ok(())
}
#[test]
fn test_many_max_size_segments() -> Result<()> {
  let mut random = random();
  let fake_directory = Arc::new(FakeDirectory::new());

  let mut policy = TieredMergePolicy::new();
  policy.set_max_merged_segment_mb(1024.0)?;

  let mut infos = SegmentInfos::new(LATEST.major)?;
  let mut i = 0;

  for _ in 0..30 {
    infos.add(make_segment_commit_info(
      &mut random,
      fake_directory.clone(),
      &format!("_{}", i),
      1000,
      0,
      1024.0,
      SOURCE_MERGE,
    )?)?;
    i += 1;
  }

  for _ in 0..8 {
    infos.add(make_segment_commit_info(
      &mut random,
      fake_directory.clone(),
      &format!("_{}", i),
      1000,
      0,
      102.0,
      SOURCE_FLUSH,
    )?)?;
    i += 1;
  }

  let merge_spec = policy.find_merges(
    MergeTrigger::SegmentFlush,
    &infos,
    None,
    &MockMergeContext::new(|s: &SegmentCommitInfo<_>| Ok(s.get_del_count())),
  )?;
  assert!(merge_spec.is_none());

  for _ in 0..5 {
    infos.add(make_segment_commit_info(
      &mut random,
      fake_directory.clone(),
      &format!("_{}", i),
      1000,
      0,
      102.0,
      SOURCE_FLUSH,
    )?)?;
    i += 1;
  }

  let merge_spec = policy.find_merges(
    MergeTrigger::SegmentFlush,
    &infos,
    None,
    &MockMergeContext::new(|s: &SegmentCommitInfo<_>| Ok(s.get_del_count())),
  )?;
  assert!(merge_spec.is_some());

  let merge_spec = merge_spec.unwrap();
  assert_eq!(1, merge_spec.merges.len());

  let merge = &merge_spec.merges[0];
  assert_eq!(10, merge.stat.segments.len());

  Ok(())
}
#[test]
fn test_merge_purely_to_reclaim_deletes() -> Result<()> {
  let mut random = random();
  let fake_directory = Arc::new(FakeDirectory::new());
  let case = TestTieredMergePolicy;

  let merge_policy = case.merge_policy::<FakeDirectory, _>(&mut random);
  let mut infos = SegmentInfos::new(LATEST.major)?;

  infos.add(make_segment_commit_info(
    &mut random,
    fake_directory.clone(),
    "_0",
    1_000_000,
    0,
    1024.0,
    SOURCE_MERGE,
  )?)?;

  let merge_spec = merge_policy.find_merges(
    MergeTrigger::Explicit,
    &infos,
    None,
    &MockMergeContext::new(|s: &SegmentCommitInfo<_>| Ok(s.get_del_count())),
  )?;
  assert!(merge_spec.is_none());

  infos = apply_deletes(infos, (0.15_f64 * 1_000_000_f64) as i32)?;
  let merge_spec = merge_policy.find_merges(
    MergeTrigger::Explicit,
    &infos,
    None,
    &MockMergeContext::new(|s: &SegmentCommitInfo<_>| Ok(s.get_del_count())),
  )?;
  assert!(merge_spec.is_none());

  infos = apply_deletes(
    infos,
    (((merge_policy.get_deletes_pct_allowed() - 15.0 + 1.0) / 100.0) * 1_000_000.0) as i32,
  )?;
  let merge_spec = merge_policy.find_merges(
    MergeTrigger::Explicit,
    &infos,
    None,
    &MockMergeContext::new(|s: &SegmentCommitInfo<_>| Ok(s.get_del_count())),
  )?;
  assert!(merge_spec.is_some());

  Ok(())
}
#[test]
fn test_merge_size_is_less_than_floor_size() -> Result<()> {
  let mut random = random();
  let fake_directory = Arc::new(FakeDirectory::new());

  let merge_context = MockMergeContext::new(|s: &SegmentCommitInfo<_>| Ok(s.get_del_count()));

  let mut infos = SegmentInfos::new(LATEST.major)?;
  for i in 0..50 {
    infos.add(make_segment_commit_info(
      &mut random,
      fake_directory.clone(),
      &format!("_{}", i),
      1_000_000,
      0,
      1.0,
      SOURCE_FLUSH,
    )?)?;
  }

  let mut merge_policy = TieredMergePolicy::new();
  merge_policy.set_max_merge_at_once(30)?;
  merge_policy.set_floor_segment_mb(0.1)?;

  let mut merge_spec =
    merge_policy.find_merges(MergeTrigger::FullFlush, &infos, None, &merge_context)?;
  assert!(merge_spec.is_some());

  let merge_spec = merge_spec.take().unwrap();
  assert_eq!(4, merge_spec.merges.len());
  for one_merge in &merge_spec.merges {
    assert_eq!(
      merge_policy.get_segments_per_tier() as usize,
      one_merge.stat.segments.len()
    );
  }

  merge_policy.set_floor_segment_mb(15.0)?;
  let mut merge_spec =
    merge_policy.find_merges(MergeTrigger::FullFlush, &infos, None, &merge_context)?;
  assert!(merge_spec.is_some());

  let merge_spec = merge_spec.take().unwrap();
  assert_eq!(3, merge_spec.merges.len());
  for one_merge in &merge_spec.merges {
    assert_eq!(15, one_merge.stat.segments.len());
  }

  merge_policy.set_floor_segment_mb(60.0)?;
  let mut merge_spec =
    merge_policy.find_merges(MergeTrigger::FullFlush, &infos, None, &merge_context)?;
  assert!(merge_spec.is_some());

  let merge_spec = merge_spec.take().unwrap();
  assert_eq!(2, merge_spec.merges.len());
  assert_eq!(30, merge_spec.merges[0].stat.segments.len());
  assert_eq!(20, merge_spec.merges[1].stat.segments.len());

  Ok(())
}

#[test]
fn test_full_flush_merges() -> Result<()> {
  let mut random = random();
  let fake_directory = Arc::new(FakeDirectory::new());

  let seg_name_generator = AtomicU64::new(0);
  let mut stats = IOStats::default();
  let merge_context = MockMergeContext::new(|s: &SegmentCommitInfo<_>| Ok(s.get_del_count()));
  let mut segment_infos = SegmentInfos::new(LATEST.major)?;

  let mp = TieredMergePolicy::new();

  for _ in 0..11 {
    segment_infos.add(make_segment_commit_info(
      &mut random,
      fake_directory.clone(),
      &format!("_{}", seg_name_generator.fetch_add(1, Ordering::SeqCst)),
      1,
      0,
      f64::MIN_POSITIVE,
      SOURCE_FLUSH,
    )?)?;
  }

  let spec = mp.find_full_flush_merges(
    MergeTrigger::FullFlush,
    &segment_infos,
    None,
    &merge_context,
  )?;
  assert!(spec.is_some());

  let spec = spec.unwrap();
  for merge in &spec.merges {
    segment_infos = apply_merge(
      &mut random,
      segment_infos,
      merge,
      &format!("_{}", seg_name_generator.fetch_add(1, Ordering::SeqCst)),
      &mut stats,
      fake_directory.clone(),
    )?;
  }

  assert_eq!(2, segment_infos.size());

  Ok(())
}
