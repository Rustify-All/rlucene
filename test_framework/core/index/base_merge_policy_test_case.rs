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
use crate::core::codecs::codec;
use crate::core::document::document::Document;
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::directory_reader;
use crate::core::index::index_reader::{Identity, IndexReader};
use crate::core::index::index_writer::{IndexWriter, Inner, SOURCE, SOURCE_FLUSH, SOURCE_MERGE};
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::merge_policy::{
  MergeContext, MergePolicy, MergePolicyEnum, MergeSpecification, OneMerge,
};
use crate::core::index::merge_scheduler::{MergeScheduler, MergeSchedulerEnum, MergeSource};
use crate::core::index::merge_trigger::MergeTrigger;
use crate::core::index::segment_commit_info::SegmentCommitInfo;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_infos::SegmentInfos;
use crate::core::index::serial_merge_scheduler::SerialMergeScheduler;
use crate::core::store::IOContext;
use crate::core::store::directory::Directory;
use crate::core::store::dummy::dummy_index_input::DummyIndexInput;
use crate::core::store::dummy::dummy_index_output::DummyIndexOutput;
use crate::core::store::dummy::dummy_lock::DummyLock;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::info_stream::InfoStreamMT;
use crate::core::util::{HasIdentity, LATEST, LUCENE_10_1_1, StringHelper};
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, new_directory_shared, new_index_writer_config_with_analyzer,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::{Rng, RngExt};
use std::collections::{HashMap, HashSet};
use std::fmt::{Debug, Display, Formatter};
use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Base test case for [`MergePolicy`]
pub trait BaseMergePolicyTestCase {
  type MergePolicy<D>: MergePolicy<D> + Into<MergePolicyEnum<D>>
  where
    D: Directory;

  fn merge_policy<D, R>(&self, random: &mut R) -> Self::MergePolicy<D>
  where
    D: Directory,
    R: Rng + ?Sized;
  fn assert_segment_infos<D>(policy: &Self::MergePolicy<D>, infos: &SegmentInfos<D>) -> Result<()>
  where
    D: Directory;
  fn assert_merge<D, CR>(
    policy: &Self::MergePolicy<D>,
    merge: &MergeSpecification<D, CR>,
  ) -> Result<()>
  where
    D: Directory,
    CR: CodecReader;

  fn test_force_merge_not_needed<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let may_merge = Arc::new(AtomicBool::new(true));

    let merge_scheduler = SerialMergeSchedulerImpl::new(may_merge.clone());

    let mut mp = self.merge_policy(random);
    if mp.to_string().contains("MockRandomMergePolicy") {
      return dir.close();
    }

    if random.random_bool(0.5) {
      mp.get_base_mut().set_no_cfs_ratio(0.0)?;
    } else {
      mp.get_base_mut().set_no_cfs_ratio(1.0)?;
    }

    let analyzer = MockAnalyzer::new(random);
    let mut iwc = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwc.set_merge_scheduler(MergeSchedulerEnum::SerialTest(merge_scheduler));
    iwc.set_merge_policy(mp);

    let writer = IndexWriter::new(dir.clone(), iwc)?;

    let num_segments = TestUtil::next_int(random, 2, 20);

    for _i in 0..num_segments {
      let num_docs = TestUtil::next_int(random, 1, 5);

      for _ in 0..num_docs {
        writer.add_document(Document::new())?;
      }

      let reader = directory_reader::open_from_writer(&writer)?;
      reader.close()?;
    }

    for i in (0..=5).rev() {
      let segment_count = writer.get_segment_count();

      let max_num_segments = if i == 0 {
        1
      } else {
        TestUtil::next_usize(random, 1, 10)
      };

      may_merge.store(segment_count > max_num_segments, Ordering::SeqCst);

      if cfg!(feature = "test_log_verbose") {
        println!(
          "TEST: now forceMerge(maxNumSegments={}) vs segmentCount={}",
          max_num_segments, segment_count
        );
      }

      writer.force_merge(max_num_segments as i32)?;
    }

    writer.close()?;

    Ok(())
  }

  fn test_find_forced_deletes_merges<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let mp = self.merge_policy(random);
    if mp.to_string().contains("MockRandomMergePolicy") {
      return Ok(());
    }

    let mut infos = SegmentInfos::new(LATEST.major)?;
    let directory = new_directory_shared(random)?;

    let context = MockMergeContext::new(|_s| Ok(0));
    let num_segs = random.random_range(0..10);

    for _ in 0..num_segs {
      let name = TestUtil::random_simple_string(random);
      let id: [u8; StringHelper::ID_LENGTH] = TestUtil::random_simple_string_range(
        random,
        StringHelper::ID_LENGTH,
        StringHelper::ID_LENGTH,
      )
      .into_bytes()
      .try_into()
      .unwrap();
      let mut info = SegmentInfo::new(
        directory.clone(),
        Some((*LATEST).clone()),
        Some((*LATEST).clone()),
        &name,
        random.random_range(0..i32::MAX),
        random.random_bool(0.5),
        false,
        Some(codec::get_default()),
        HashMap::new(),
        id,
        HashMap::new(),
        None,
      )?;
      info.set_files(HashSet::new())?;

      infos.add(SegmentCommitInfo::new(
        info,
        random.random_range(0..1),
        0,
        -1,
        -1,
        -1,
        Some(StringHelper::random_id()),
      ))?;
    }

    let forced_deletes_merges = mp.find_forced_deletes_merges(&infos, None, &context)?;

    if let Some(forced_deletes_merges) = forced_deletes_merges {
      assert_eq!(0, forced_deletes_merges.merges.len());
    }

    Ok(())
  }

  fn test_simulate_append_only<D, R>(
    &self,
    random: &mut R,
    merge_policy: &Self::MergePolicy<D>,
    fake_directory: Arc<D>,
  ) -> Result<()>
  where
    D: Directory,
    R: Rng + ?Sized,
  {
    self.do_test_simulate_append_only(random, merge_policy, fake_directory, 100_000_000, 10_000)
  }

  fn do_test_simulate_append_only<D, R>(
    &self,
    random: &mut R,
    merge_policy: &Self::MergePolicy<D>,
    fake_directory: Arc<D>,
    total_docs: i32,
    max_docs_per_flush: i32,
  ) -> Result<()>
  where
    D: Directory,
    R: Rng + ?Sized,
  {
    let mut stats = IOStats::default();
    let seg_name_generator = AtomicU64::new(0);

    let merge_context = MockMergeContext::new(|s| Ok(s.get_del_count()));
    let mut segment_infos = SegmentInfos::new(LATEST.major)?;

    let avg_doc_size_mb = 5.0 / 1024.0; // 5kB

    let mut num_docs = 0i32;

    while num_docs < total_docs {
      let flush_doc_count = TestUtil::next_int(random, 1, max_docs_per_flush);
      num_docs += flush_doc_count;

      let flush_size_mb = (flush_doc_count as f64) * avg_doc_size_mb;
      stats.flush_bytes_written += (flush_size_mb * 1024.0 * 1024.0) as i64;

      let name = format!("_{}", seg_name_generator.fetch_add(1, Ordering::SeqCst));

      segment_infos.add(make_segment_commit_info(
        random,
        fake_directory.clone(),
        &name,
        flush_doc_count,
        0,
        flush_size_mb,
        SOURCE_FLUSH,
      )?)?;

      let mut merges = merge_policy.find_full_flush_merges(
        MergeTrigger::SegmentFlush,
        &segment_infos,
        None,
        &merge_context,
      )?;

      if merges.is_none() {
        merges = merge_policy.find_merges(
          MergeTrigger::SegmentFlush,
          &segment_infos,
          None,
          &merge_context,
        )?;
      }

      while let Some(spec) = merges {
        assert!(!spec.merges.is_empty());
        Self::assert_merge(merge_policy, &spec)?;
        for one_merge in &spec.merges {
          let name = format!("_{}", seg_name_generator.fetch_add(1, Ordering::SeqCst));

          segment_infos = apply_merge(
            random,
            segment_infos,
            one_merge,
            &name,
            &mut stats,
            fake_directory.clone(),
          )?;
        }

        merges = merge_policy.find_merges(
          MergeTrigger::MergeFinished,
          &segment_infos,
          None,
          &merge_context,
        )?;
      }

      Self::assert_segment_infos(merge_policy, &segment_infos)?;
    }

    if cfg!(feature = "test_log_verbose") {
      let wa = (stats.flush_bytes_written + stats.merge_bytes_written) as f64
        / (stats.flush_bytes_written as f64);

      println!("Write amplification for append-only: {}", wa);
    }

    Ok(())
  }
  fn test_simulate_updates<D, R>(
    &self,
    random: &mut R,
    merge_policy: &Self::MergePolicy<D>,
    fake_directory: Arc<D>,
  ) -> Result<()>
  where
    D: Directory,
    R: Rng + ?Sized,
  {
    let num_docs = at_least(random, 1_000_000);
    self.do_test_simulate_updates(random, merge_policy, fake_directory, num_docs, 2500)
  }

  fn do_test_simulate_updates<D, R>(
    &self,
    random: &mut R,
    merge_policy: &Self::MergePolicy<D>,
    fake_directory: Arc<D>,
    total_docs: i32,
    max_docs_per_flush: i32,
  ) -> Result<()>
  where
    D: Directory,
    R: Rng + ?Sized,
  {
    let mut stats = IOStats::default();
    let seg_name_generator = AtomicU64::new(0);

    let merge_context = MockMergeContext::new(|s| Ok(s.get_del_count()));
    let mut segment_infos = SegmentInfos::new(LATEST.major)?;

    let avg_doc_size_mb = 5.0 / 1024.0; // 5kB

    let mut num_docs = 0i32;

    while num_docs < total_docs {
      let flush_doc_count = if random.random_bool(0.9) {
        TestUtil::next_int(random, max_docs_per_flush / 2, max_docs_per_flush)
      } else {
        TestUtil::next_int(random, 1, max_docs_per_flush)
      };

      let del_count =
        ((flush_doc_count as f64) * 0.9 * (num_docs as f64) / (total_docs as f64)) as i32;

      num_docs += flush_doc_count - del_count;

      segment_infos = apply_deletes(segment_infos, del_count)?;

      let flush_size_mb = (flush_doc_count as f64) * avg_doc_size_mb;
      stats.flush_bytes_written += (flush_size_mb * 1024.0 * 1024.0) as i64;

      let name = format!("_{}", seg_name_generator.fetch_add(1, Ordering::SeqCst));

      segment_infos.add(make_segment_commit_info(
        random,
        fake_directory.clone(),
        &name,
        flush_doc_count,
        0,
        flush_size_mb,
        SOURCE_FLUSH,
      )?)?;

      let mut merges = merge_policy.find_full_flush_merges(
        MergeTrigger::SegmentFlush,
        &segment_infos,
        None,
        &merge_context,
      )?;

      if merges.is_none() {
        merges = merge_policy.find_merges(
          MergeTrigger::SegmentFlush,
          &segment_infos,
          None,
          &merge_context,
        )?;
      }

      while let Some(spec) = merges {
        Self::assert_merge(merge_policy, &spec)?;

        for one_merge in &spec.merges {
          let name = format!("_{}", seg_name_generator.fetch_add(1, Ordering::SeqCst));

          segment_infos = apply_merge(
            random,
            segment_infos,
            one_merge,
            &name,
            &mut stats,
            fake_directory.clone(),
          )?;
        }

        merges = merge_policy.find_merges(
          MergeTrigger::MergeFinished,
          &segment_infos,
          None,
          &merge_context,
        )?;
      }

      Self::assert_segment_infos(merge_policy, &segment_infos)?;
    }

    if cfg!(feature = "test_log_verbose") {
      let wa = (stats.flush_bytes_written + stats.merge_bytes_written) as f64
        / (stats.flush_bytes_written as f64);

      println!("Write amplification for update: {}", wa);

      let mut total_del_count = 0i32;
      let mut total_max_doc = 0i32;

      for i in 0..segment_infos.size() {
        let sci = segment_infos.info(i).unwrap();
        total_del_count += sci.get_del_count();
        total_max_doc += sci.info.max_doc()?;
      }

      let live_ratio = 1.0 - (total_del_count as f64) / (total_max_doc as f64);

      println!("Final live ratio: {}", live_ratio);
    }

    Ok(())
  }
  fn test_no_pathological_merges<D, R>(
    &self,
    random: &mut R,
    merge_policy: &Self::MergePolicy<D>,
    fake_directory: Arc<D>,
  ) -> Result<()>
  where
    D: Directory,
    R: Rng + ?Sized,
  {
    let mut stats = IOStats::default();
    let seg_name_generator = AtomicU64::new(0);

    let merge_context = MockMergeContext::new(|s| Ok(s.get_del_count()));
    let mut segment_infos = SegmentInfos::new(LATEST.major)?;

    let avg_doc_size_mb = 10.0 / 1024.0 / 1024.0;
    let max_docs_per_flush = 3;
    let total_docs = 10_000;

    let mut num_flushes = 0i32;
    let mut num_docs = 0i32;

    while num_docs < total_docs {
      let flush_doc_count = TestUtil::next_int(random, 1, max_docs_per_flush);

      num_docs += flush_doc_count;

      let flush_size_mb = (flush_doc_count as f64) * avg_doc_size_mb;
      stats.flush_bytes_written += (flush_size_mb * 1024.0 * 1024.0) as i64;

      let name = format!("_{}", seg_name_generator.fetch_add(1, Ordering::SeqCst));

      segment_infos.add(make_segment_commit_info(
        random,
        fake_directory.clone(),
        &name,
        flush_doc_count,
        0,
        flush_size_mb,
        SOURCE_FLUSH,
      )?)?;

      num_flushes += 1;

      let mut merges = merge_policy.find_merges(
        MergeTrigger::SegmentFlush,
        &segment_infos,
        None,
        &merge_context,
      )?;

      while let Some(spec) = merges {
        assert!(!spec.merges.is_empty());

        Self::assert_merge(merge_policy, &spec)?;

        for one_merge in &spec.merges {
          let name = format!("_{}", seg_name_generator.fetch_add(1, Ordering::SeqCst));

          segment_infos = apply_merge(
            random,
            segment_infos,
            one_merge,
            &name,
            &mut stats,
            fake_directory.clone(),
          )?;
        }

        merges = merge_policy.find_merges(
          MergeTrigger::MergeFinished,
          &segment_infos,
          None,
          &merge_context,
        )?;
      }

      Self::assert_segment_infos(merge_policy, &segment_infos)?;
    }

    let write_amplification = (stats.flush_bytes_written + stats.merge_bytes_written) as f64
      / (stats.flush_bytes_written as f64);

    let max_allowed_write_amplification = (num_flushes as f64).ln() / (1.5f64).ln();

    assert!(write_amplification < max_allowed_write_amplification);

    Ok(())
  }
}
pub(crate) fn make_segment_commit_info<R, D>(
  random: &mut R,
  fake_directory: Arc<D>,
  name: &str,
  max_doc: i32,
  num_deleted_docs: i32,
  size_mb: f64,
  source: &str,
) -> Result<SegmentCommitInfo<D>>
where
  R: Rng + ?Sized,
  D: Directory,
{
  if !name.starts_with('_') {
    return Err(LuceneError::illegal_argument(format!(
      "name must start with an _, got {}",
      name
    )));
  }

  let mut id = [0u8; StringHelper::ID_LENGTH];
  random.fill(&mut id);

  let mut diagnostics = HashMap::new();
  diagnostics.insert(SOURCE.to_string(), source.to_string());

  let mut info = SegmentInfo::new(
    fake_directory,
    Some((*LATEST).clone()),
    Some((*LATEST).clone()),
    name,
    max_doc,
    false,
    false,
    Some(codec::get_default()),
    HashMap::new(),
    id,
    diagnostics,
    None,
  )?;

  let size = (size_mb * 1024.0 * 1024.0) as i64;
  let file_name = format!("{}_size={}.fake", name, size);

  let mut files = HashSet::new();
  files.insert(file_name);
  info.set_files(files)?;

  Ok(SegmentCommitInfo::new(
    info,
    num_deleted_docs,
    0,
    0,
    0,
    0,
    Some(StringHelper::random_id()),
  ))
}
pub(crate) fn apply_merge<D, CR, R>(
  random: &mut R,
  mut infos: SegmentInfos<D>,
  merge: &OneMerge<D, CR>,
  merged_segment_name: &str,
  stats: &mut IOStats,
  fake_directory: Arc<D>,
) -> Result<SegmentInfos<D>>
where
  D: Directory,
  CR: CodecReader,
  R: Rng + ?Sized,
{
  let mut new_max_doc = 0i32;
  let mut new_size_mb = 0f64;
  let mut merged_away = vec![false; infos.size()];
  let mut merged_ids: Vec<_> = merge.stat.segments.iter().map(String::as_str).collect();
  merged_ids.sort_unstable();

  let mut num_merged_segments = 0;
  for (index, sci) in infos.iter().iter().enumerate() {
    if merged_ids.binary_search(&sci.info.get_id_key()).is_err() {
      continue;
    }

    num_merged_segments += 1;
    merged_away[index] = true;
    let max_doc = sci.info.max_doc()?;
    let num_live_docs = max_doc - sci.get_del_count();

    if max_doc > 0 {
      new_size_mb +=
        (sci.size_in_bytes()? as f64) * (num_live_docs as f64) / (max_doc as f64) / 1024.0 / 1024.0;
    }

    new_max_doc += num_live_docs;
  }
  assert_eq!(merge.stat.segments.len(), num_merged_segments);

  let merged_info = make_segment_commit_info(
    random,
    fake_directory,
    merged_segment_name,
    new_max_doc,
    0,
    new_size_mb,
    SOURCE_MERGE,
  )?;

  let mut merged_segment_added = false;
  let mut new_infos = SegmentInfos::new(LATEST.major)?;

  for (index, info) in infos.segments.drain(..).enumerate() {
    if merged_away[index] {
      if !merged_segment_added {
        new_infos.add(merged_info.clone())?;
        merged_segment_added = true;
      }
    } else {
      new_infos.add(info)?;
    }
  }

  stats.merge_bytes_written += (new_size_mb * 1024.0 * 1024.0) as i64;

  Ok(new_infos)
}
pub(crate) fn apply_deletes<D>(
  infos: SegmentInfos<D>,
  mut num_deletes: i32,
) -> Result<SegmentInfos<D>>
where
  D: Directory,
{
  let mut total_num_docs = 0i32;

  for i in 0..infos.size() {
    let sci = infos.info(i).unwrap();
    total_num_docs += sci.info.max_doc()? - sci.get_del_count();
  }

  if num_deletes > total_num_docs {
    return Err(LuceneError::illegal_argument("More deletes than documents"));
  }

  let w = num_deletes as f64 / total_num_docs as f64;

  let mut new_infos = SegmentInfos::new(LATEST.major)?;

  for i in 0..infos.size() {
    debug_assert!(num_deletes >= 0);

    let sci = infos.info(i).unwrap();
    let live_docs = sci.info.max_doc()? - sci.get_del_count();

    let seg_deletes = if i == infos.size() - 1 {
      num_deletes
    } else {
      let v = (w * (live_docs as f64)).ceil() as i32;
      std::cmp::min(num_deletes, v)
    };

    let new_del_count = sci.get_del_count() + seg_deletes;

    debug_assert!(new_del_count <= sci.info.max_doc()?);

    if new_del_count < sci.info.max_doc()? {
      let dummy = SegmentInfo::new(
        sci.info.dir.clone(),
        Some((*LUCENE_10_1_1).clone()),
        Some((*LUCENE_10_1_1).clone()),
        "_0",
        1,
        false,
        false,
        Some(codec::get_default()),
        HashMap::new(),
        StringHelper::random_id(),
        HashMap::new(),
        None,
      )?;
      let mut new_info = SegmentCommitInfo::new(
        dummy,
        new_del_count,
        0,
        sci.get_del_gen() + 1,
        sci.get_field_infos_gen(),
        sci.get_doc_values_gen(),
        Some(StringHelper::random_id()),
      );
      new_info.info = sci.info.clone();
      new_infos.add(new_info)?;
    }

    num_deletes -= seg_deletes;
  }

  debug_assert!(num_deletes == 0);

  Ok(new_infos)
}
#[derive(Debug, Default, Clone)]
pub struct IOStats {
  /// Bytes written through flushes.
  pub(crate) flush_bytes_written: i64,

  /// Bytes written through merges.
  pub(crate) merge_bytes_written: i64,
}
pub struct MockMergeContext<D, F> {
  num_deletes_func: F,
  dir: PhantomData<D>,
  merging_segments: HashSet<String>,
  info_stream: InfoStreamMT,
}

impl<D, F> MockMergeContext<D, F>
where
  D: Directory,
  F: Fn(&SegmentCommitInfo<D>) -> Result<i32>,
{
  pub fn new(num_deletes_func: F) -> Self {
    Self {
      num_deletes_func,
      dir: PhantomData,
      merging_segments: HashSet::new(),
      info_stream: InfoStreamMT::default(),
    }
  }
  pub fn set_merging_segments(&mut self, merging_segments: HashSet<String>) {
    self.merging_segments = merging_segments;
  }
}
impl<D, F> MergeContext<D> for MockMergeContext<D, F>
where
  D: Directory,
  F: Fn(&SegmentCommitInfo<D>) -> Result<i32>,
{
  fn num_deletes_to_merge(&self, info: &SegmentCommitInfo<D>) -> Result<i32> {
    (self.num_deletes_func)(info)
  }

  fn num_deleted_docs(&self, info: &SegmentCommitInfo<D>) -> i32 {
    self.num_deletes_to_merge(info).unwrap()
  }

  fn get_info_stream(&self) -> InfoStreamMT {
    self.info_stream.clone()
  }

  fn get_merging_segments(&self, _inner: Option<&Inner<D>>) -> HashSet<String> {
    self.merging_segments.clone()
  }
}

pub(crate) struct FakeDirectory {
  id: Identity,
}
impl FakeDirectory {
  pub fn new() -> Self {
    Self {
      id: Identity::new(),
    }
  }
}

impl Display for FakeDirectory {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}

impl CloseableRef for FakeDirectory {
  fn close(&self) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }
}

impl HasIdentity for FakeDirectory {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl Directory for FakeDirectory {
  fn list_all(&self) -> Result<Vec<String>> {
    Ok(vec![])
  }

  fn delete_file(&self, _name: &str) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn file_length(&self, name: &str) -> Result<usize> {
    if name.ends_with(".liv") {
      return Ok(0);
    }

    if !name.ends_with(".fake") {
      return Err(LuceneError::illegal_argument(name.to_string()));
    }

    let marker = "_size=";
    let start_index = name
      .find(marker)
      .ok_or_else(|| LuceneError::illegal_argument(name.to_string()))?
      + marker.len();

    let end_index = name.len() - ".fake".len();

    let size = name[start_index..end_index]
      .parse::<i64>()
      .map_err(|_| LuceneError::illegal_argument(name.to_string()))?;

    Ok(size as usize)
  }

  fn create_output(&self, _name: &str, _context: &IOContext) -> Result<Self::IndexOutput> {
    Err(LuceneError::unsupported_operation(""))
  }

  type IndexOutput = DummyIndexOutput;

  fn create_temp_output(
    &self,
    _prefix: &str,
    _suffix: &str,
    _context: &IOContext,
  ) -> Result<Self::IndexOutput> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn sync(&self, _names: &[String]) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn sync_metadata(&self) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn rename(&self, _source: &str, _dest: &str) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }

  type IndexInput = DummyIndexInput;

  fn open_input(&self, _name: &str, _context: &IOContext) -> Result<Self::IndexInput> {
    Err(LuceneError::unsupported_operation(""))
  }

  type Lock = DummyLock;

  fn obtain_lock(&self, _name: &str) -> Result<Self::Lock> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn get_pending_deletions(&self) -> Result<HashSet<String>> {
    Err(LuceneError::unsupported_operation(""))
  }
}
pub struct SerialMergeSchedulerImpl {
  may_merge: Arc<AtomicBool>,
  base: SerialMergeScheduler,
}

impl SerialMergeSchedulerImpl {
  pub(crate) fn new(may_merge: Arc<AtomicBool>) -> Self {
    Self {
      may_merge,
      base: SerialMergeScheduler::new(),
    }
  }
}

impl CloseableRef for SerialMergeSchedulerImpl {
  fn close(&self) -> Result<()> {
    self.base.close()
  }
}

impl MergeScheduler for SerialMergeSchedulerImpl {
  fn merge<MS, D>(&self, merge_source: MS, trigger: MergeTrigger) -> Result<()>
  where
    MS: MergeSource<D> + Clone + 'static,
    D: Directory + 'static,
    OneMerge<D, MS::Reader>: Send + 'static,
  {
    if !self.may_merge.load(Ordering::SeqCst) {
      let merge = merge_source.get_next_merge()?;
      if merge.is_some() {
        return Err(LuceneError::illegal_argument(
          "TEST: we should not need any merging, yet merge policy returned merge",
        ));
      }
    }
    self.base.merge(merge_source, trigger)
  }

  type Directory<D>
    = <SerialMergeScheduler as MergeScheduler>::Directory<D>
  where
    D: Directory;

  fn wrap_for_merge<D>(&self, in_: D) -> Result<Self::Directory<D>>
  where
    D: Directory,
  {
    self.base.wrap_for_merge(in_)
  }

  fn initialize<D>(
    &mut self,
    info_stream: crate::core::util::info_stream::InfoStreamMT,
    directory: &D,
  ) -> Result<()>
  where
    D: Directory,
  {
    self.base.initialize(info_stream, directory)
  }
}
