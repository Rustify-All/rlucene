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
use crate::core::document::string_field::StringField;
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::concurrent_merge_scheduler::{
  ConcurrentMergeScheduler, ConcurrentMergeSchedulerBase, ConcurrentMergeSchedulerDefaults,
};
use crate::core::index::dummy::dummy_doc_map_sorter::DummyDocMap;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_writer::{IndexWriter, Inner};
use crate::core::index::merge_policy::{
  DefaultMergeSpecification, MergeContext, MergePolicy, MergePolicyBase, MergePolicyEnum,
  MergeReader, MergeSpecification, MergeStat, OneMerge, OneMergeBase, OneMergeDefaults,
  OneMergeHook, OneMergeSR,
};
use crate::core::index::merge_scheduler::MergeSource;
use crate::core::index::merge_trigger::MergeTrigger;
use crate::core::index::one_merge_wrapping_merge_policy::{
  OneMergeUnaryOperatorBase, OneMergeWrappingMergePolicy,
};
use crate::core::index::segment_commit_info::SegmentCommitInfo;
use crate::core::index::segment_infos::SegmentInfos;
use crate::core::index::segment_reader::{DefaultLeafReader, SegmentReader};
use crate::core::store::directory::Directory;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::index::test_concurrent_merge_scheduler::CountDownLatch;
use crate::test_framework::core::util::lucene_test_case::{
  new_field, new_index_writer_config_with_analyzer, new_text_field, random,
};
use rand::Rng;
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
pub static STORED_TEXT_TYPE: LazyLock<FieldType> = LazyLock::new(|| {
  FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)
    .expect("should not fail")
});
#[allow(dead_code)]
struct TestIndexWriter;

#[derive(Clone)]
pub struct CloseWhileMergeIsRunningConcurrentMergeScheduler {
  merge_started: CountDownLatch,
  close_started: CountDownLatch,
}

impl CloseWhileMergeIsRunningConcurrentMergeScheduler {
  pub fn new(merge_started: CountDownLatch, close_started: CountDownLatch) -> Self {
    Self {
      merge_started,
      close_started,
    }
  }
}

impl ConcurrentMergeSchedulerBase for CloseWhileMergeIsRunningConcurrentMergeScheduler {
  fn close(&self, _scheduler: &ConcurrentMergeScheduler) -> Result<()> {
    Ok(())
  }

  fn do_merge<MS, D>(
    &self,
    scheduler: &ConcurrentMergeScheduler,
    merge_source: &MS,
    merge: OneMerge<D, MS::Reader>,
  ) -> Result<()>
  where
    MS: MergeSource<D>,
    D: Directory + 'static,
  {
    self.merge_started.count_down();
    self.close_started.wait();
    ConcurrentMergeSchedulerDefaults::do_merge(scheduler, merge_source, merge)
  }
}

pub(crate) fn add_doc<D, R>(
  random: &mut R,
  writer: &IndexWriter<D>,
  field_types: &mut HashMap<String, FieldType>,
) -> crate::core::util::error::lucene_error::Result<()>
where
  D: Directory + 'static,
  R: Rng + ?Sized,
{
  let mut doc = Document::new();
  doc.add(new_text_field(
    random,
    "content",
    "aaa",
    Store::No,
    field_types,
  )?);
  let _ = writer.add_document(doc)?;
  Ok(())
}
pub(crate) fn add_doc_with_index<D, R>(
  random: &mut R,
  writer: &IndexWriter<D>,
  index: i32,
  field_types: &mut HashMap<String, FieldType>,
) -> crate::core::util::error::lucene_error::Result<()>
where
  D: Directory + 'static,
  R: Rng + ?Sized,
{
  let mut doc = Document::new();
  doc.add(new_field(
    random,
    "content",
    format!("aaa {}", index),
    &STORED_TEXT_TYPE,
    field_types,
  )?);
  doc.add(StringField::from_string(
    "id",
    index.to_string(),
    Store::No,
  )?);

  match writer.add_document(doc) {
    Ok(_) => Ok(()),
    Err(e) => Err(e),
  }
}

pub(crate) fn assert_no_unreferenced_files<D>(
  dir: Arc<D>,
  message: &str,
) -> crate::core::util::error::lucene_error::Result<()>
where
  D: Directory + 'static,
{
  let mut start_files = dir.list_all()?;
  let mut random = random();
  let mock = MockAnalyzer::new(&mut random);
  let writer = IndexWriter::new(
    dir.clone(),
    new_index_writer_config_with_analyzer(&mut random, mock)?,
  )?;
  writer.close()?;
  let mut end_files = dir.list_all()?;

  start_files.sort();
  end_files.sort();

  assert_eq!(
    start_files,
    end_files,
    "{}: before delete:\n    {}\n  after delete:\n    {}",
    message,
    start_files.join("\n    "),
    end_files.join("\n    ")
  );

  Ok(())
}

pub struct SoftUpdatesConcurrentlyMergePolicy<D>
where
  D: Directory,
{
  base: OneMergeWrappingMergePolicy<D>,
  merge_away_soft_deletes: Arc<AtomicBool>,
}

impl<D> Clone for SoftUpdatesConcurrentlyMergePolicy<D>
where
  D: Directory,
{
  fn clone(&self) -> Self {
    Self {
      base: self.base.clone(),
      merge_away_soft_deletes: self.merge_away_soft_deletes.clone(),
    }
  }
}

impl<D> SoftUpdatesConcurrentlyMergePolicy<D>
where
  D: Directory,
{
  pub(crate) fn new(
    base: OneMergeWrappingMergePolicy<D>,
    merge_away_soft_deletes: Arc<AtomicBool>,
  ) -> Self {
    Self {
      base,
      merge_away_soft_deletes,
    }
  }
}

impl<D> From<SoftUpdatesConcurrentlyMergePolicy<D>> for MergePolicyEnum<D>
where
  D: Directory,
{
  fn from(value: SoftUpdatesConcurrentlyMergePolicy<D>) -> Self {
    Self::SoftUpdatesConcurrently(value)
  }
}

impl<D> Display for SoftUpdatesConcurrentlyMergePolicy<D>
where
  D: Directory,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", self.base)
  }
}

impl<D> MergePolicy<D> for SoftUpdatesConcurrentlyMergePolicy<D>
where
  D: Directory,
{
  fn get_base(&self) -> &MergePolicyBase {
    self.base.get_base()
  }

  fn get_base_mut(&mut self) -> &mut MergePolicyBase {
    self.base.get_base_mut()
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
    self
      .base
      .find_merges(merge_trigger, segment_infos, inner, merge_context)
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
    self.base.max_full_flush_merge_size()
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
    if self.merge_away_soft_deletes.load(Ordering::SeqCst) {
      self
        .base
        .num_deletes_to_merge(info, del_count, reader_supplier)
    } else {
      Ok(0)
    }
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

pub struct SoftUpdatesConcurrentlyOneMergeUnaryOperator {
  merge_away_soft_deletes: Arc<AtomicBool>,
}

impl Clone for SoftUpdatesConcurrentlyOneMergeUnaryOperator {
  fn clone(&self) -> Self {
    Self {
      merge_away_soft_deletes: self.merge_away_soft_deletes.clone(),
    }
  }
}

impl SoftUpdatesConcurrentlyOneMergeUnaryOperator {
  pub(crate) fn new(merge_away_soft_deletes: Arc<AtomicBool>) -> Self {
    Self {
      merge_away_soft_deletes,
    }
  }
}

impl<D> OneMergeUnaryOperatorBase<D> for SoftUpdatesConcurrentlyOneMergeUnaryOperator
where
  D: Directory,
{
  fn apply(&self, mut merge: OneMergeSR<D>) -> Result<OneMergeSR<D>> {
    let wrapped = merge.replace_hook(OneMergeHook::Default);
    Ok(
      OneMerge::new(merge.segments)?.with_hook(OneMergeHook::SoftUpdatesConcurrently(
        SoftUpdatesConcurrentlyOneMerge::new(
          wrapped,
          self.merge_away_soft_deletes.clone(),
          soft_updates_all_live_docs,
        ),
      )),
    )
  }
}

pub(crate) struct SoftUpdatesConcurrentlyOneMerge<D, CR>
where
  D: Directory,
  CR: CodecReader,
{
  wrapped: Box<OneMergeHook<D, CR>>,
  merge_away_soft_deletes: Arc<AtomicBool>,
  all_live_docs: fn(CR) -> Result<CR>,
}

impl<D, CR> SoftUpdatesConcurrentlyOneMerge<D, CR>
where
  D: Directory,
  CR: CodecReader,
{
  fn new(
    wrapped: OneMergeHook<D, CR>,
    merge_away_soft_deletes: Arc<AtomicBool>,
    all_live_docs: fn(CR) -> Result<CR>,
  ) -> Self {
    Self {
      wrapped: Box::new(wrapped),
      merge_away_soft_deletes,
      all_live_docs,
    }
  }
}

impl<D, CR> OneMergeBase<D, CR> for SoftUpdatesConcurrentlyOneMerge<D, CR>
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
    let wrapped = self.wrapped.wrap_for_merge(reader)?;
    if self.merge_away_soft_deletes.load(Ordering::SeqCst) {
      Ok(wrapped)
    } else {
      (self.all_live_docs)(wrapped)
    }
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
    OneMergeDefaults::init_merge_readers(merge_readers, stat, reader_factory)
  }
}

fn soft_updates_all_live_docs<D>(reader: DefaultLeafReader<D>) -> Result<DefaultLeafReader<D>>
where
  D: Directory,
{
  let max_doc = reader.max_doc()?;
  Ok(Arc::new(SegmentReader::new_from_reader(
    reader.get_segment_info(),
    reader.as_ref(),
    None,
    None,
    max_doc,
    true,
  )?))
}

#[derive(Clone)]
pub struct MergeFinishedOnceOneMergeUnaryOperator;

impl<D> OneMergeUnaryOperatorBase<D> for MergeFinishedOnceOneMergeUnaryOperator
where
  D: Directory,
{
  fn apply(&self, merge: OneMergeSR<D>) -> Result<OneMergeSR<D>> {
    Ok(
      OneMerge::new(merge.segments)?.with_hook(OneMergeHook::MergeFinishedOnce(
        MergeFinishedOnceOneMerge::new(),
      )),
    )
  }
}

pub(crate) struct MergeFinishedOnceOneMerge {
  only_finish_once: AtomicBool,
}

impl MergeFinishedOnceOneMerge {
  fn new() -> Self {
    Self {
      only_finish_once: AtomicBool::new(false),
    }
  }
}

impl<D, CR> OneMergeBase<D, CR> for MergeFinishedOnceOneMerge
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
    OneMergeDefaults::merge_finished(inner, stat, success, segment_dropped)?;
    if self.only_finish_once.swap(true, Ordering::SeqCst) {
      return Err(LuceneError::illegal_state(
        "mergeFinished may only be called once",
      ));
    }
    Ok(())
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
    OneMergeDefaults::init_merge_readers(merge_readers, stat, reader_factory)
  }
}

#[derive(Clone)]
pub struct AbortOnMergeCompleteOneMergeUnaryOperator {
  abort_merge_before_commit: Arc<AtomicBool>,
}

impl AbortOnMergeCompleteOneMergeUnaryOperator {
  pub(crate) fn new(abort_merge_before_commit: Arc<AtomicBool>) -> Self {
    Self {
      abort_merge_before_commit,
    }
  }
}

impl<D> OneMergeUnaryOperatorBase<D> for AbortOnMergeCompleteOneMergeUnaryOperator
where
  D: Directory,
{
  fn apply(&self, merge: OneMergeSR<D>) -> Result<OneMergeSR<D>> {
    Ok(
      OneMerge::new(merge.segments)?.with_hook(OneMergeHook::AbortOnMergeComplete(
        AbortOnMergeCompleteOneMerge::new(self.abort_merge_before_commit.clone()),
      )),
    )
  }
}

pub(crate) struct AbortOnMergeCompleteOneMerge {
  abort_merge_before_commit: Arc<AtomicBool>,
}

impl AbortOnMergeCompleteOneMerge {
  fn new(abort_merge_before_commit: Arc<AtomicBool>) -> Self {
    Self {
      abort_merge_before_commit,
    }
  }
}

impl<D, CR> OneMergeBase<D, CR> for AbortOnMergeCompleteOneMerge
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
    OneMergeDefaults::set_merge_info(stat, merge_info, info)
  }

  fn on_merge_complete(
    &self,
    inner: &mut Inner<D>,
    stat: &MergeStat,
    merge_info: &Option<SegmentCommitInfo<D>>,
    is_aborted: bool,
  ) -> Result<()> {
    OneMergeDefaults::on_merge_complete(inner, stat, merge_info, is_aborted)?;
    if self.abort_merge_before_commit.load(Ordering::SeqCst) {
      stat.set_aborted();
    }
    Ok(())
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
