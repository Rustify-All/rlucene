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
use crate::codec::memory::direct_postings_format::DirectPostingsFormat;
use crate::core::codecs::Codec;
use crate::core::codecs::postings_format::PostingsFormat;
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::dummy::dummy_doc_map_sorter::DummyDocMap;
use crate::core::index::index_writer::Inner;
use crate::core::index::merge_policy::{
  DefaultMergeSpecification, MergeContext, MergePolicy, MergePolicyBase, MergePolicyEnum,
  MergeReader, MergeSpecification, MergeStat, OneMerge, OneMergeBase, OneMergeDefaults,
  OneMergeHook,
};
use crate::core::index::merge_scheduler::{MergeScheduler, MergeSchedulerEnum, MergeSource};
use crate::core::index::merge_trigger::MergeTrigger;
use crate::core::index::segment_commit_info::SegmentCommitInfo;
use crate::core::index::segment_infos::SegmentInfos;
use crate::core::index::tiered_merge_policy::TieredMergePolicy;
use crate::core::store::directory::Directory;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::codecs::asserting_codec::{
  AssertingCodecBase, AssertingCodecDefaults, AssertingCodecDocValuesFormat,
  AssertingCodecKnnVectorsFormat, AssertingCodecPostingsFormat,
};
use crate::test_framework::core::util::test_util::{DefaultCodec, TestUtil};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[allow(dead_code)] // for quick search
struct TestAddIndexes;

pub(crate) struct CustomPerFieldAssertingCodec {
  defaults: AssertingCodecDefaults,
  direct_format: AssertingCodecPostingsFormat,
  default_format: AssertingCodecPostingsFormat,
}

impl CustomPerFieldAssertingCodec {
  pub(crate) fn new() -> Result<Self> {
    Ok(Self {
      defaults: AssertingCodecDefaults::default(),
      direct_format: AssertingCodecPostingsFormat::Direct(DirectPostingsFormat::for_name(
        "Direct",
      )?),
      default_format: TestUtil::get_default_postings_format().into(),
    })
  }
}

impl AssertingCodecBase for CustomPerFieldAssertingCodec {
  fn get_postings_format_for_field(&self, field: &str) -> Result<&AssertingCodecPostingsFormat> {
    if field == "id" {
      Ok(&self.direct_format)
    } else {
      Ok(&self.default_format)
    }
  }

  fn get_doc_values_format_for_field(&self, field: &str) -> Result<&AssertingCodecDocValuesFormat> {
    self.defaults.get_doc_values_format_for_field(field)
  }

  fn get_knn_vectors_format_for_field(
    &self,
    field: &str,
  ) -> Result<&AssertingCodecKnnVectorsFormat> {
    self.defaults.get_knn_vectors_format_for_field(field)
  }
}

#[derive(Clone)]
pub struct UnRegisteredCodec {
  delegate: DefaultCodec,
}

impl UnRegisteredCodec {
  pub fn new() -> Self {
    Self {
      delegate: TestUtil::get_default_codec(),
    }
  }
}

impl Display for UnRegisteredCodec {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "NotRegistered({})", self.delegate)
  }
}

impl Codec for UnRegisteredCodec {
  type PostingsFormat = <DefaultCodec as Codec>::PostingsFormat;
  type DocValuesFormat = <DefaultCodec as Codec>::DocValuesFormat;
  type StoredFieldsFormat = <DefaultCodec as Codec>::StoredFieldsFormat;
  type TermVectorsFormat = <DefaultCodec as Codec>::TermVectorsFormat;
  type FieldInfosFormat = <DefaultCodec as Codec>::FieldInfosFormat;
  type SegmentInfoFormat = <DefaultCodec as Codec>::SegmentInfoFormat;
  type NormsFormat = <DefaultCodec as Codec>::NormsFormat;
  type LiveDocsFormat = <DefaultCodec as Codec>::LiveDocsFormat;
  type CompoundFormat = <DefaultCodec as Codec>::CompoundFormat;
  type PointsFormat = <DefaultCodec as Codec>::PointsFormat;
  type KnnVectorsFormat = <DefaultCodec as Codec>::KnnVectorsFormat;

  fn postings_format(&self) -> Self::PostingsFormat {
    self.delegate.postings_format()
  }

  fn doc_values_format(&self) -> Self::DocValuesFormat {
    self.delegate.doc_values_format()
  }

  fn stored_fields_format(&self) -> Self::StoredFieldsFormat {
    self.delegate.stored_fields_format()
  }

  fn term_vectors_format(&self) -> Self::TermVectorsFormat {
    self.delegate.term_vectors_format()
  }

  fn field_infos_format(&self) -> Self::FieldInfosFormat {
    self.delegate.field_infos_format()
  }

  fn segment_info_format(&self) -> Self::SegmentInfoFormat {
    self.delegate.segment_info_format()
  }

  fn norms_format(&self) -> Self::NormsFormat {
    self.delegate.norms_format()
  }

  fn live_docs_format(&self) -> Self::LiveDocsFormat {
    self.delegate.live_docs_format()
  }

  fn compound_format(&self) -> Self::CompoundFormat {
    self.delegate.compound_format()
  }

  fn points_format(&self) -> Self::PointsFormat {
    self.delegate.points_format()
  }

  fn knn_vectors_format(&self) -> Result<Self::KnnVectorsFormat> {
    self.delegate.knn_vectors_format()
  }

  fn get_name(&self) -> &str {
    "NotRegistered"
  }
}

#[derive(Clone, Default)]
pub struct ConcurrentAddIndexesMergePolicy {
  base: TieredMergePolicy,
  merge_specification: AddIndexesMergeSpecification,
}

#[derive(Clone, Default)]
enum AddIndexesMergeSpecification {
  #[default]
  Concurrent,
  None,
  Empty,
  Diagnostics,
}

impl ConcurrentAddIndexesMergePolicy {
  pub fn null_merge_specification() -> Self {
    Self {
      base: TieredMergePolicy::default(),
      merge_specification: AddIndexesMergeSpecification::None,
    }
  }

  pub fn empty_merge_specification() -> Self {
    Self {
      base: TieredMergePolicy::default(),
      merge_specification: AddIndexesMergeSpecification::Empty,
    }
  }

  pub fn diagnostics_merge_specification() -> Self {
    Self {
      base: TieredMergePolicy::default(),
      merge_specification: AddIndexesMergeSpecification::Diagnostics,
    }
  }
}

impl<D> From<ConcurrentAddIndexesMergePolicy> for MergePolicyEnum<D>
where
  D: Directory,
{
  fn from(value: ConcurrentAddIndexesMergePolicy) -> Self {
    Self::ConcurrentAddIndexes(value)
  }
}

impl Display for ConcurrentAddIndexesMergePolicy {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "ConcurrentAddIndexesMergePolicy")
  }
}

impl<D> MergePolicy<D> for ConcurrentAddIndexesMergePolicy
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
    match self.merge_specification {
      AddIndexesMergeSpecification::Concurrent => {
        // Create a OneMerge for each reader to let them get concurrently processed by addIndexes().
        let mut merge_spec = MergeSpecification::new();
        for reader in readers {
          merge_spec.add(OneMerge::from_codec_readers(vec![reader])?);
        }
        Ok(Some(merge_spec))
      },
      AddIndexesMergeSpecification::None => Ok(None),
      AddIndexesMergeSpecification::Empty => Ok(Some(MergeSpecification::new())),
      AddIndexesMergeSpecification::Diagnostics => {
        let mut merge_spec = MergeSpecification::new();
        merge_spec.add(
          OneMerge::from_codec_readers(readers)?
            .with_hook(OneMergeHook::SetDiagnostics(SetDiagnosticsOneMerge::new())),
        );
        Ok(Some(merge_spec))
      },
    }
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

  fn size<MC>(&self, info: &SegmentCommitInfo<D>, merge_context: &MC) -> Result<i64>
  where
    MC: MergeContext<D>,
  {
    self.base.size(info, merge_context)
  }

  fn max_full_flush_merge_size(&self) -> i64 {
    MergePolicy::<D>::max_full_flush_merge_size(&self.base)
  }
}

pub(crate) struct SetDiagnosticsOneMerge;

impl SetDiagnosticsOneMerge {
  pub(crate) fn new() -> Self {
    Self
  }
}

impl<D, CR> OneMergeBase<D, CR> for SetDiagnosticsOneMerge
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
    mut info: SegmentCommitInfo<D>,
  ) {
    Arc::get_mut(&mut info.info)
      .expect("new addIndexes SegmentInfo must be uniquely owned")
      .add_diagnostics(HashMap::from([(
        "merge_policy".to_string(),
        "my_merge_policy".to_string(),
      )]));
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

pub struct PartialMergeScheduler {
  merges_to_do: usize,
  merges_triggered: AtomicUsize,
}

impl PartialMergeScheduler {
  pub fn new(merges_to_do: usize) -> Self {
    Self {
      merges_to_do,
      merges_triggered: AtomicUsize::new(0),
    }
  }
}

impl Clone for PartialMergeScheduler {
  fn clone(&self) -> Self {
    Self {
      merges_to_do: self.merges_to_do,
      merges_triggered: AtomicUsize::new(self.merges_triggered.load(Ordering::Relaxed)),
    }
  }
}

impl From<PartialMergeScheduler> for MergeSchedulerEnum {
  fn from(value: PartialMergeScheduler) -> Self {
    Self::PartialAddIndexes(value)
  }
}

impl CloseableRef for PartialMergeScheduler {}

impl MergeScheduler for PartialMergeScheduler {
  fn merge<MS, D>(&self, merge_source: MS, _trigger: MergeTrigger) -> Result<()>
  where
    MS: MergeSource<D> + Clone + 'static,
    D: Directory + 'static,
    OneMerge<D, MS::Reader>: Send + 'static,
  {
    loop {
      let Some(mut merge) = merge_source.get_next_merge()? else {
        break;
      };
      if self.merges_triggered.load(Ordering::Relaxed) >= self.merges_to_do {
        let merge_stat = merge.stat.clone();
        merge.close_for_test(false, false, |_| Ok(()))?;
        merge_source.on_merge_finished(&merge_stat, None);
      } else {
        merge_source.merge(merge)?;
        self.merges_triggered.fetch_add(1, Ordering::Relaxed);
      }
    }
    Ok(())
  }

  type Directory<D>
    = D
  where
    D: Directory;

  fn wrap_for_merge<D>(&self, in_: D) -> Result<Self::Directory<D>>
  where
    D: Directory,
  {
    Ok(in_)
  }
}

#[derive(Clone, Default)]
pub struct CountingSerialMergeScheduler {
  explicit_merges: Arc<AtomicUsize>,
  add_indexes_merges: Arc<AtomicUsize>,
}

impl CountingSerialMergeScheduler {
  pub fn explicit_merges(&self) -> usize {
    self.explicit_merges.load(Ordering::Relaxed)
  }

  pub fn add_indexes_merges(&self) -> usize {
    self.add_indexes_merges.load(Ordering::Relaxed)
  }
}

impl From<CountingSerialMergeScheduler> for MergeSchedulerEnum {
  fn from(value: CountingSerialMergeScheduler) -> Self {
    Self::CountingAddIndexes(value)
  }
}

impl CloseableRef for CountingSerialMergeScheduler {}

impl MergeScheduler for CountingSerialMergeScheduler {
  fn merge<MS, D>(&self, merge_source: MS, trigger: MergeTrigger) -> Result<()>
  where
    MS: MergeSource<D> + Clone + 'static,
    D: Directory + 'static,
    OneMerge<D, MS::Reader>: Send + 'static,
  {
    while let Some(merge) = merge_source.get_next_merge()? {
      merge_source.merge(merge)?;
      if trigger == MergeTrigger::Explicit {
        self.explicit_merges.fetch_add(1, Ordering::Relaxed);
      }
      if trigger == MergeTrigger::AddIndexes {
        self.add_indexes_merges.fetch_add(1, Ordering::Relaxed);
      }
    }
    Ok(())
  }

  type Directory<D>
    = D
  where
    D: Directory;

  fn wrap_for_merge<D>(&self, in_: D) -> Result<Self::Directory<D>>
  where
    D: Directory,
  {
    Ok(in_)
  }
}
