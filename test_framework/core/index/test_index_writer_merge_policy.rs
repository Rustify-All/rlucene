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
use crate::core::index::concurrent_merge_scheduler::{
  ConcurrentMergeScheduler, ConcurrentMergeSchedulerBase, ConcurrentMergeSchedulerDefaults,
};
use crate::core::index::dummy::dummy_doc_map_sorter::DummyDocMap;
use crate::core::index::index_writer::Inner;
use crate::core::index::log_doc_merge_policy::LogDocMergePolicy;
use crate::core::index::log_merge_policy::LogMergePolicy;
use crate::core::index::merge_policy::{
  DefaultMergeSpecification, MergeContext, MergePolicy, MergePolicyBase, MergePolicyEnum,
  MergeReader, MergeSpecification, MergeStat, OneMerge, OneMergeBase, OneMergeDefaults,
  OneMergeHook, OneMergeSR,
};
use crate::core::index::merge_scheduler::{MergeScheduler, MergeSource};
use crate::core::index::merge_trigger::MergeTrigger;
use crate::core::index::one_merge_wrapping_merge_policy::OneMergeUnaryOperatorBase;
use crate::core::index::segment_commit_info::SegmentCommitInfo;
use crate::core::index::segment_infos::SegmentInfos;
use crate::core::index::segment_reader::DefaultLeafReader;
use crate::core::index::serial_merge_scheduler::SerialMergeScheduler;
use crate::core::store::directory::Directory;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::index::test_concurrent_merge_scheduler::CountDownLatch;
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

#[allow(dead_code)] // for quick search
struct TestIndexWriterMergePolicy;

#[derive(Clone)]
pub struct MergeDvUpdateFileOnGetReaderConcurrentMergeScheduler {
  wait_for_init_merge_reader: CountDownLatch,
  wait_for_dv_update: CountDownLatch,
}

impl MergeDvUpdateFileOnGetReaderConcurrentMergeScheduler {
  pub fn new(
    wait_for_init_merge_reader: CountDownLatch,
    wait_for_dv_update: CountDownLatch,
  ) -> Self {
    Self {
      wait_for_init_merge_reader,
      wait_for_dv_update,
    }
  }
}

impl ConcurrentMergeSchedulerBase for MergeDvUpdateFileOnGetReaderConcurrentMergeScheduler {
  fn merge<MS, D>(
    &self,
    scheduler: &ConcurrentMergeScheduler,
    merge_source: MS,
    trigger: MergeTrigger,
  ) -> Result<()>
  where
    MS: MergeSource<D> + Clone + 'static,
    D: Directory + 'static,
    OneMerge<D, MS::Reader>: Send + 'static,
  {
    self.wait_for_init_merge_reader.count_down();
    self.wait_for_dv_update.wait();
    ConcurrentMergeSchedulerDefaults::merge(scheduler, merge_source, trigger)
  }
}

#[derive(Clone)]
pub struct MergeDvUpdateFileOnCommitConcurrentMergeScheduler {
  wait_for_init_merge_reader: CountDownLatch,
  wait_for_dv_update: CountDownLatch,
}

impl MergeDvUpdateFileOnCommitConcurrentMergeScheduler {
  pub fn new(
    wait_for_init_merge_reader: CountDownLatch,
    wait_for_dv_update: CountDownLatch,
  ) -> Self {
    Self {
      wait_for_init_merge_reader,
      wait_for_dv_update,
    }
  }
}

impl ConcurrentMergeSchedulerBase for MergeDvUpdateFileOnCommitConcurrentMergeScheduler {
  fn merge<MS, D>(
    &self,
    scheduler: &ConcurrentMergeScheduler,
    merge_source: MS,
    trigger: MergeTrigger,
  ) -> Result<()>
  where
    MS: MergeSource<D> + Clone + 'static,
    D: Directory + 'static,
    OneMerge<D, MS::Reader>: Send + 'static,
  {
    self.wait_for_init_merge_reader.count_down();
    self.wait_for_dv_update.wait();
    ConcurrentMergeSchedulerDefaults::merge(scheduler, merge_source, trigger)
  }
}

#[derive(Clone)]
pub struct ForceMergeDvUpdateOneMergeUnaryOperator {
  wait_for_init_merge_reader: CountDownLatch,
  wait_for_dv_update: CountDownLatch,
}

impl ForceMergeDvUpdateOneMergeUnaryOperator {
  pub fn new(
    wait_for_init_merge_reader: CountDownLatch,
    wait_for_dv_update: CountDownLatch,
  ) -> Self {
    Self {
      wait_for_init_merge_reader,
      wait_for_dv_update,
    }
  }
}

impl<D> OneMergeUnaryOperatorBase<D> for ForceMergeDvUpdateOneMergeUnaryOperator
where
  D: Directory,
{
  fn apply(&self, merge: OneMergeSR<D>) -> Result<OneMergeSR<D>> {
    Ok(
      OneMerge::new(merge.segments)?.with_hook(OneMergeHook::ForceMergeDvUpdate(
        ForceMergeDvUpdateOneMerge::new(
          self.wait_for_init_merge_reader.clone(),
          self.wait_for_dv_update.clone(),
        ),
      )),
    )
  }
}

pub(crate) struct ForceMergeDvUpdateOneMerge {
  wait_for_init_merge_reader: CountDownLatch,
  wait_for_dv_update: CountDownLatch,
}

impl ForceMergeDvUpdateOneMerge {
  fn new(wait_for_init_merge_reader: CountDownLatch, wait_for_dv_update: CountDownLatch) -> Self {
    Self {
      wait_for_init_merge_reader,
      wait_for_dv_update,
    }
  }
}

impl<D, CR> OneMergeBase<D, CR> for ForceMergeDvUpdateOneMerge
where
  D: Directory,
  CR: CodecReader,
{
  fn merge_finished(
    &self,
    inner: &mut Inner<D>,
    stat: &MergeStat,
    success: bool,
    segment_dropped: bool,
  ) -> Result<()> {
    OneMergeDefaults::merge_finished(inner, stat, success, segment_dropped)
  }

  fn wrap_for_merge(&self, reader: CR) -> Result<CR> {
    self.wait_for_dv_update.wait();
    OneMergeDefaults::wrap_for_merge(reader)
  }

  fn reorder<CR1, D1>(&self, reader: &CR1, dir: D1) -> Result<Option<DummyDocMap>>
  where
    CR1: CodecReader,
    D1: Directory,
  {
    OneMergeDefaults::reorder(reader, dir)
  }

  fn set_merge_info(
    &self,
    stat: &MergeStat,
    merge_info: &mut Option<SegmentCommitInfo<D>>,
    info: SegmentCommitInfo<D>,
  ) {
    OneMergeDefaults::set_merge_info(stat, merge_info, info)
  }

  fn on_merge_complete(
    &self,
    inner: &mut Inner<D>,
    stat: &MergeStat,
    merge_info: &Option<SegmentCommitInfo<D>>,
    is_aborted: bool,
  ) -> Result<()> {
    OneMergeDefaults::on_merge_complete(inner, stat, merge_info, is_aborted)
  }

  fn init_merge_readers<F>(
    &self,
    merge_readers: &mut Vec<MergeReader<CR>>,
    stat: &MergeStat,
    reader_factory: F,
  ) -> Result<()>
  where
    F: FnMut(&String) -> Result<MergeReader<CR>>,
  {
    OneMergeDefaults::init_merge_readers(merge_readers, stat, reader_factory)?;
    self.wait_for_init_merge_reader.count_down();
    Ok(())
  }
}

#[derive(Clone)]
pub struct TestLatch {
  inner: Arc<(Mutex<bool>, Condvar)>,
}

impl TestLatch {
  pub fn new() -> Self {
    Self {
      inner: Arc::new((Mutex::new(false), Condvar::new())),
    }
  }

  pub fn count_down(&self) {
    let (lock, cvar) = &*self.inner;
    *lock.lock().expect("test latch mutex poisoned") = true;
    cvar.notify_all();
  }

  pub fn wait(&self) {
    let (lock, cvar) = &*self.inner;
    let mut signaled = lock.lock().expect("test latch mutex poisoned");
    while !*signaled {
      signaled = cvar.wait(signaled).expect("test latch mutex poisoned");
    }
  }

  pub fn wait_timeout(&self, timeout: Duration) -> bool {
    let (lock, cvar) = &*self.inner;
    let signaled = lock.lock().expect("test latch mutex poisoned");
    let (signaled, _) = cvar
      .wait_timeout_while(signaled, timeout, |signaled| !*signaled)
      .expect("test latch mutex poisoned");
    *signaled
  }
}

impl Default for TestLatch {
  fn default() -> Self {
    Self::new()
  }
}

pub struct LatchedSerialMergeScheduler {
  merge_started: TestLatch,
  merge_released: TestLatch,
  base: SerialMergeScheduler,
}

impl LatchedSerialMergeScheduler {
  pub fn new(merge_started: TestLatch, merge_released: TestLatch) -> Self {
    Self {
      merge_started,
      merge_released,
      base: SerialMergeScheduler::new(),
    }
  }
}

impl CloseableRef for LatchedSerialMergeScheduler {
  fn close(&self) -> Result<()> {
    self.base.close()
  }
}

impl MergeScheduler for LatchedSerialMergeScheduler {
  fn merge<MS, D>(&self, merge_source: MS, trigger: MergeTrigger) -> Result<()>
  where
    MS: MergeSource<D> + Clone + 'static,
    D: Directory + 'static,
    OneMerge<D, MS::Reader>: Send + 'static,
  {
    self.merge_started.count_down();
    self.merge_released.wait();
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

pub struct SetDiagnosticsMergePolicy<D>
where
  D: Directory,
{
  in_: Box<MergePolicyEnum<D>>,
}

impl<D> Clone for SetDiagnosticsMergePolicy<D>
where
  D: Directory,
{
  fn clone(&self) -> Self {
    Self {
      in_: self.in_.clone(),
    }
  }
}

impl<D> SetDiagnosticsMergePolicy<D>
where
  D: Directory,
{
  pub fn new<T>(in_: T) -> Self
  where
    T: Into<MergePolicyEnum<D>>,
  {
    Self {
      in_: Box::new(in_.into()),
    }
  }

  fn wrap_specification(
    &self,
    spec: Option<DefaultMergeSpecification<D>>,
  ) -> Option<DefaultMergeSpecification<D>> {
    spec.map(|spec| {
      let mut new_spec = DefaultMergeSpecification::new();
      for merge in spec.merges {
        new_spec.add(merge.with_hook(OneMergeHook::SetMergePolicyDiagnostics(
          SetMergePolicyDiagnosticsOneMerge::new(),
        )));
      }
      new_spec
    })
  }
}

pub(crate) struct SetMergePolicyDiagnosticsOneMerge;

impl SetMergePolicyDiagnosticsOneMerge {
  pub(crate) fn new() -> Self {
    Self
  }
}

impl<D, CR> OneMergeBase<D, CR> for SetMergePolicyDiagnosticsOneMerge
where
  D: Directory,
  CR: CodecReader,
{
  fn merge_finished(
    &self,
    inner: &mut Inner<D>,
    stat: &MergeStat,
    success: bool,
    segment_dropped: bool,
  ) -> Result<()> {
    OneMergeDefaults::merge_finished(inner, stat, success, segment_dropped)
  }

  fn wrap_for_merge(&self, reader: CR) -> Result<CR> {
    OneMergeDefaults::wrap_for_merge(reader)
  }

  fn reorder<CR1, D1>(&self, reader: &CR1, dir: D1) -> Result<Option<DummyDocMap>>
  where
    CR1: CodecReader,
    D1: Directory,
  {
    OneMergeDefaults::reorder(reader, dir)
  }

  fn set_merge_info(
    &self,
    stat: &MergeStat,
    merge_info: &mut Option<SegmentCommitInfo<D>>,
    info: SegmentCommitInfo<D>,
  ) {
    OneMergeDefaults::set_merge_info(stat, merge_info, info);
    Arc::get_mut(
      &mut merge_info
        .as_mut()
        .expect("super.setMergeInfo must set mergeInfo")
        .info,
    )
    .expect("new merged SegmentInfo must be uniquely owned")
    .add_diagnostics(HashMap::from([(
      "merge_policy".to_string(),
      "my_merge_policy".to_string(),
    )]));
  }

  fn on_merge_complete(
    &self,
    inner: &mut Inner<D>,
    stat: &MergeStat,
    merge_info: &Option<SegmentCommitInfo<D>>,
    is_aborted: bool,
  ) -> Result<()> {
    OneMergeDefaults::on_merge_complete(inner, stat, merge_info, is_aborted)
  }

  fn init_merge_readers<F>(
    &self,
    merge_readers: &mut Vec<MergeReader<CR>>,
    stat: &MergeStat,
    reader_factory: F,
  ) -> Result<()>
  where
    F: FnMut(&String) -> Result<MergeReader<CR>>,
  {
    OneMergeDefaults::init_merge_readers(merge_readers, stat, reader_factory)
  }
}

impl<D> From<SetDiagnosticsMergePolicy<D>> for MergePolicyEnum<D>
where
  D: Directory,
{
  fn from(value: SetDiagnosticsMergePolicy<D>) -> Self {
    Self::SetDiagnostics(value)
  }
}

impl<D> Display for SetDiagnosticsMergePolicy<D>
where
  D: Directory,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "SetDiagnosticsMergePolicy({})", self.in_)
  }
}

impl<D> MergePolicy<D> for SetDiagnosticsMergePolicy<D>
where
  D: Directory,
{
  fn get_base(&self) -> &MergePolicyBase {
    self.in_.get_base()
  }

  fn get_base_mut(&mut self) -> &mut MergePolicyBase {
    self.in_.get_base_mut()
  }

  fn find_merges<MC>(
    &self,
    merge_trigger: MergeTrigger,
    segment_infos: &SegmentInfos<D>,
    inner: Option<&Inner<D>>,
    merge_context: &MC,
  ) -> Result<Option<DefaultMergeSpecification<D>>>
  where
    MC: MergeContext<D>,
  {
    Ok(self.wrap_specification(self.in_.find_merges(
      merge_trigger,
      segment_infos,
      inner,
      merge_context,
    )?))
  }

  fn find_merges_readers<CR>(&self, readers: Vec<CR>) -> Result<Option<MergeSpecification<D, CR>>>
  where
    CR: CodecReader,
  {
    self.in_.find_merges_readers(readers)
  }

  fn find_forced_merges<MC>(
    &self,
    segment_infos: &SegmentInfos<D>,
    max_segment_count: usize,
    segments_to_merge: &HashMap<String, Option<bool>>,
    inner: Option<&Inner<D>>,
    merge_context: &MC,
  ) -> Result<Option<DefaultMergeSpecification<D>>>
  where
    MC: MergeContext<D>,
  {
    self.in_.find_forced_merges(
      segment_infos,
      max_segment_count,
      segments_to_merge,
      inner,
      merge_context,
    )
  }

  fn find_forced_deletes_merges<MC>(
    &self,
    segment_infos: &SegmentInfos<D>,
    inner: Option<&Inner<D>>,
    merge_context: &MC,
  ) -> Result<Option<DefaultMergeSpecification<D>>>
  where
    MC: MergeContext<D>,
  {
    self
      .in_
      .find_forced_deletes_merges(segment_infos, inner, merge_context)
  }

  fn find_full_flush_merges<MC>(
    &self,
    merge_trigger: MergeTrigger,
    segment_infos: &SegmentInfos<D>,
    inner: Option<&Inner<D>>,
    merge_context: &MC,
  ) -> Result<Option<DefaultMergeSpecification<D>>>
  where
    MC: MergeContext<D>,
  {
    Ok(self.wrap_specification(self.in_.find_full_flush_merges(
      merge_trigger,
      segment_infos,
      inner,
      merge_context,
    )?))
  }

  fn use_compound_file<MC>(
    &self,
    infos: &SegmentInfos<D>,
    merged_info: &SegmentCommitInfo<D>,
    merge_context: &MC,
  ) -> Result<bool>
  where
    MC: MergeContext<D>,
  {
    self
      .in_
      .use_compound_file(infos, merged_info, merge_context)
  }

  fn size<MC>(&self, info: &SegmentCommitInfo<D>, merge_context: &MC) -> Result<i64>
  where
    MC: MergeContext<D>,
  {
    self.in_.size(info, merge_context)
  }

  fn max_full_flush_merge_size(&self) -> i64 {
    self.in_.max_full_flush_merge_size()
  }

  fn has_merged<MC>(
    &self,
    infos: &SegmentInfos<D>,
    info: &SegmentCommitInfo<D>,
    merge_context: &MC,
  ) -> Result<bool>
  where
    MC: MergeContext<D>,
  {
    self.in_.has_merged(infos, info, merge_context)
  }

  fn keep_fully_deleted_segment<F>(&self, reader_supplier: F) -> Result<bool>
  where
    F: Fn() -> Result<DefaultLeafReader<D>>,
  {
    self.in_.keep_fully_deleted_segment(reader_supplier)
  }

  fn num_deletes_to_merge<F>(
    &self,
    info: &SegmentCommitInfo<D>,
    del_count: i32,
    reader_supplier: &F,
  ) -> Result<i32>
  where
    F: Fn() -> Result<DefaultLeafReader<D>>,
  {
    self
      .in_
      .num_deletes_to_merge(info, del_count, reader_supplier)
  }

  fn seg_string<MC>(&self, merge_context: &MC, infos: &[SegmentCommitInfo<D>]) -> String
  where
    MC: MergeContext<D>,
  {
    self.in_.seg_string(merge_context, infos)
  }

  fn message<MC>(&self, message: &str, merge_context: &MC) -> Result<()>
  where
    MC: MergeContext<D>,
  {
    self.in_.message(message, merge_context)
  }

  fn verbose<MC>(&self, merge_context: &MC) -> bool
  where
    MC: MergeContext<D>,
  {
    self.in_.verbose(merge_context)
  }
}

#[derive(Clone)]
pub struct ForceMergeDvUpdateMergePolicy {
  base: LogMergePolicy<LogDocMergePolicy>,
}

#[allow(clippy::new_without_default)]
impl ForceMergeDvUpdateMergePolicy {
  pub fn new() -> Self {
    Self {
      base: LogMergePolicy::log_doc(),
    }
  }
}

impl<D> From<ForceMergeDvUpdateMergePolicy> for MergePolicyEnum<D>
where
  D: Directory,
{
  fn from(value: ForceMergeDvUpdateMergePolicy) -> Self {
    Self::ForceMergeDvUpdate(value)
  }
}

impl Display for ForceMergeDvUpdateMergePolicy {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", self.base)
  }
}

impl<D> MergePolicy<D> for ForceMergeDvUpdateMergePolicy
where
  D: Directory,
{
  fn get_base(&self) -> &MergePolicyBase {
    MergePolicy::<D>::get_base(&self.base)
  }

  fn get_base_mut(&mut self) -> &mut MergePolicyBase {
    MergePolicy::<D>::get_base_mut(&mut self.base)
  }

  fn find_merges<MC>(
    &self,
    _merge_trigger: MergeTrigger,
    _segment_infos: &SegmentInfos<D>,
    _inner: Option<&Inner<D>>,
    _merge_context: &MC,
  ) -> Result<Option<DefaultMergeSpecification<D>>>
  where
    MC: MergeContext<D>,
  {
    // Only allow force merge.
    Ok(None)
  }

  fn find_merges_readers<CR>(&self, readers: Vec<CR>) -> Result<Option<MergeSpecification<D, CR>>>
  where
    CR: CodecReader,
  {
    self.base.find_merges_readers(readers)
  }

  fn find_forced_merges<MC>(
    &self,
    segment_infos: &SegmentInfos<D>,
    max_segment_count: usize,
    segments_to_merge: &HashMap<String, Option<bool>>,
    inner: Option<&Inner<D>>,
    merge_context: &MC,
  ) -> Result<Option<DefaultMergeSpecification<D>>>
  where
    MC: MergeContext<D>,
  {
    self.base.find_forced_merges(
      segment_infos,
      max_segment_count,
      segments_to_merge,
      inner,
      merge_context,
    )
  }

  fn find_forced_deletes_merges<MC>(
    &self,
    segment_infos: &SegmentInfos<D>,
    inner: Option<&Inner<D>>,
    merge_context: &MC,
  ) -> Result<Option<DefaultMergeSpecification<D>>>
  where
    MC: MergeContext<D>,
  {
    self
      .base
      .find_forced_deletes_merges(segment_infos, inner, merge_context)
  }

  fn find_full_flush_merges<MC>(
    &self,
    merge_trigger: MergeTrigger,
    segment_infos: &SegmentInfos<D>,
    inner: Option<&Inner<D>>,
    merge_context: &MC,
  ) -> Result<Option<DefaultMergeSpecification<D>>>
  where
    MC: MergeContext<D>,
  {
    self
      .base
      .find_full_flush_merges(merge_trigger, segment_infos, inner, merge_context)
  }

  fn use_compound_file<MC>(
    &self,
    infos: &SegmentInfos<D>,
    merged_info: &SegmentCommitInfo<D>,
    merge_context: &MC,
  ) -> Result<bool>
  where
    MC: MergeContext<D>,
  {
    self
      .base
      .use_compound_file(infos, merged_info, merge_context)
  }

  fn size<MC>(&self, info: &SegmentCommitInfo<D>, merge_context: &MC) -> Result<i64>
  where
    MC: MergeContext<D>,
  {
    self.base.size(info, merge_context)
  }

  fn max_full_flush_merge_size(&self) -> i64 {
    MergePolicy::<D>::max_full_flush_merge_size(&self.base)
  }

  fn has_merged<MC>(
    &self,
    infos: &SegmentInfos<D>,
    info: &SegmentCommitInfo<D>,
    merge_context: &MC,
  ) -> Result<bool>
  where
    MC: MergeContext<D>,
  {
    self.base.has_merged(infos, info, merge_context)
  }

  fn keep_fully_deleted_segment<F>(&self, reader_supplier: F) -> Result<bool>
  where
    F: Fn() -> Result<DefaultLeafReader<D>>,
  {
    self.base.keep_fully_deleted_segment(reader_supplier)
  }

  fn num_deletes_to_merge<F>(
    &self,
    info: &SegmentCommitInfo<D>,
    del_count: i32,
    reader_supplier: &F,
  ) -> Result<i32>
  where
    F: Fn() -> Result<DefaultLeafReader<D>>,
  {
    self
      .base
      .num_deletes_to_merge(info, del_count, reader_supplier)
  }

  fn seg_string<MC>(&self, merge_context: &MC, infos: &[SegmentCommitInfo<D>]) -> String
  where
    MC: MergeContext<D>,
  {
    self.base.seg_string(merge_context, infos)
  }

  fn message<MC>(&self, message: &str, merge_context: &MC) -> Result<()>
  where
    MC: MergeContext<D>,
  {
    self.base.message(message, merge_context)
  }

  fn verbose<MC>(&self, merge_context: &MC) -> bool
  where
    MC: MergeContext<D>,
  {
    self.base.verbose(merge_context)
  }
}
