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
use crate::core::index::index_writer::{Inner, PointInTimeOneMerge};
#[cfg(test)]
use crate::core::index::merge_policy::OneMerge;
use crate::core::index::merge_policy::{
  DefaultMergeSpecification, MergeContext, MergePolicy, MergePolicyBase, MergePolicyEnum,
  MergeSpecification, OneMergeSR,
};
use crate::core::index::merge_trigger::MergeTrigger;
use crate::core::index::segment_commit_info::SegmentCommitInfo;
use crate::core::index::segment_infos::SegmentInfos;
use crate::core::index::segment_reader::DefaultLeafReader;
use crate::core::index::soft_deletes_retention_merge_policy::SoftDeletesRetentionOneMergeUnaryOperator;
use crate::core::store::directory::Directory;
use crate::core::util::error::lucene_error::Result;
#[cfg(test)]
use crate::test_framework::core::index::test_index_writer::{
  AbortOnMergeCompleteOneMergeUnaryOperator, MergeFinishedOnceOneMergeUnaryOperator,
  SoftUpdatesConcurrentlyOneMergeUnaryOperator,
};
#[cfg(test)]
use crate::test_framework::core::index::test_index_writer_merge_policy::ForceMergeDvUpdateOneMergeUnaryOperator;
#[cfg(test)]
use crate::test_framework::core::index::test_one_merge_wrapping_merge_policy::WrappedOneMergeUnaryOperator;
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
/// A wrapping merge policy that wraps the `OneMerge` objects returned by the
/// wrapped merge policy.
///
/// # Experimental
///
/// This API is experimental and may change in incompatible ways.
pub struct OneMergeWrappingMergePolicy<D>
where
  D: Directory,
{
  in_: Box<MergePolicyEnum<D>>,
  wrap_one_merge: OneMergeUnaryOperator<D>,
}

impl<D> Clone for OneMergeWrappingMergePolicy<D>
where
  D: Directory,
{
  fn clone(&self) -> Self {
    Self {
      in_: self.in_.clone(),
      wrap_one_merge: self.wrap_one_merge.clone(),
    }
  }
}

impl<D> OneMergeWrappingMergePolicy<D>
where
  D: Directory,
{
  pub fn new<T, W>(in_: T, wrap_one_merge: W) -> Self
  where
    T: Into<MergePolicyEnum<D>>,
    W: Into<OneMergeUnaryOperator<D>>,
  {
    Self {
      in_: Box::new(in_.into()),
      wrap_one_merge: wrap_one_merge.into(),
    }
  }

  fn wrap_spec(
    &self,
    spec: Option<DefaultMergeSpecification<D>>,
  ) -> Result<Option<DefaultMergeSpecification<D>>> {
    spec
      .map(|spec| {
        let mut wrapped = DefaultMergeSpecification::new();
        for merge in spec.merges {
          wrapped.add(self.wrap_one_merge.apply(merge)?);
        }
        Ok(wrapped)
      })
      .transpose()
  }
}

pub enum OneMergeUnaryOperator<D>
where
  D: Directory,
{
  Identity(IdentityOneMergeUnaryOperator),
  PointInTime(Box<PointInTimeOneMerge<D, DefaultLeafReader<D>>>),
  SoftDeletesRetention(SoftDeletesRetentionOneMergeUnaryOperator<D>),
  #[cfg(test)]
  NewOneMerge(NewOneMergeUnaryOperator),
  #[cfg(test)]
  MergeFinishedOnce(MergeFinishedOnceOneMergeUnaryOperator),
  #[cfg(test)]
  AbortOnMergeComplete(AbortOnMergeCompleteOneMergeUnaryOperator),
  #[cfg(test)]
  ForceMergeDvUpdate(ForceMergeDvUpdateOneMergeUnaryOperator),
  #[cfg(test)]
  SoftUpdatesConcurrently(SoftUpdatesConcurrentlyOneMergeUnaryOperator),
  #[cfg(test)]
  Wrapped(WrappedOneMergeUnaryOperator),
}

impl<D> Clone for OneMergeUnaryOperator<D>
where
  D: Directory,
{
  fn clone(&self) -> Self {
    match self {
      Self::Identity(operator) => Self::Identity(operator.clone()),
      Self::PointInTime(operator) => Self::PointInTime(operator.clone()),
      Self::SoftDeletesRetention(operator) => Self::SoftDeletesRetention(operator.clone()),
      #[cfg(test)]
      Self::NewOneMerge(operator) => Self::NewOneMerge(operator.clone()),
      #[cfg(test)]
      Self::MergeFinishedOnce(operator) => Self::MergeFinishedOnce(operator.clone()),
      #[cfg(test)]
      Self::AbortOnMergeComplete(operator) => Self::AbortOnMergeComplete(operator.clone()),
      #[cfg(test)]
      Self::ForceMergeDvUpdate(operator) => Self::ForceMergeDvUpdate(operator.clone()),
      #[cfg(test)]
      Self::SoftUpdatesConcurrently(operator) => Self::SoftUpdatesConcurrently(operator.clone()),
      #[cfg(test)]
      Self::Wrapped(operator) => Self::Wrapped(operator.clone()),
    }
  }
}

pub trait OneMergeUnaryOperatorBase<D>
where
  D: Directory,
{
  fn apply(&self, merge: OneMergeSR<D>) -> Result<OneMergeSR<D>>;
}

impl<D> OneMergeUnaryOperatorBase<D> for OneMergeUnaryOperator<D>
where
  D: Directory,
{
  fn apply(&self, merge: OneMergeSR<D>) -> Result<OneMergeSR<D>> {
    match self {
      Self::Identity(operator) => operator.apply(merge),
      Self::PointInTime(operator) => operator.apply(merge),
      Self::SoftDeletesRetention(operator) => operator.apply(merge),
      #[cfg(test)]
      Self::NewOneMerge(operator) => operator.apply(merge),
      #[cfg(test)]
      Self::MergeFinishedOnce(operator) => operator.apply(merge),
      #[cfg(test)]
      Self::AbortOnMergeComplete(operator) => operator.apply(merge),
      #[cfg(test)]
      Self::ForceMergeDvUpdate(operator) => operator.apply(merge),
      #[cfg(test)]
      Self::SoftUpdatesConcurrently(operator) => operator.apply(merge),
      #[cfg(test)]
      Self::Wrapped(operator) => operator.apply(merge),
    }
  }
}

#[derive(Clone)]
pub struct IdentityOneMergeUnaryOperator;

impl<D> From<IdentityOneMergeUnaryOperator> for OneMergeUnaryOperator<D>
where
  D: Directory,
{
  fn from(value: IdentityOneMergeUnaryOperator) -> Self {
    Self::Identity(value)
  }
}

impl<D> From<PointInTimeOneMerge<D, DefaultLeafReader<D>>> for OneMergeUnaryOperator<D>
where
  D: Directory,
{
  fn from(value: PointInTimeOneMerge<D, DefaultLeafReader<D>>) -> Self {
    Self::PointInTime(Box::new(value))
  }
}

impl<D> OneMergeUnaryOperatorBase<D> for IdentityOneMergeUnaryOperator
where
  D: Directory,
{
  fn apply(&self, merge: OneMergeSR<D>) -> Result<OneMergeSR<D>> {
    Ok(merge)
  }
}

#[cfg(test)]
#[derive(Clone)]
pub struct NewOneMergeUnaryOperator;

#[cfg(test)]
impl<D> From<NewOneMergeUnaryOperator> for OneMergeUnaryOperator<D>
where
  D: Directory,
{
  fn from(value: NewOneMergeUnaryOperator) -> Self {
    Self::NewOneMerge(value)
  }
}

#[cfg(test)]
impl<D> From<MergeFinishedOnceOneMergeUnaryOperator> for OneMergeUnaryOperator<D>
where
  D: Directory,
{
  fn from(value: MergeFinishedOnceOneMergeUnaryOperator) -> Self {
    Self::MergeFinishedOnce(value)
  }
}

#[cfg(test)]
impl<D> From<AbortOnMergeCompleteOneMergeUnaryOperator> for OneMergeUnaryOperator<D>
where
  D: Directory,
{
  fn from(value: AbortOnMergeCompleteOneMergeUnaryOperator) -> Self {
    Self::AbortOnMergeComplete(value)
  }
}

#[cfg(test)]
impl<D> From<ForceMergeDvUpdateOneMergeUnaryOperator> for OneMergeUnaryOperator<D>
where
  D: Directory,
{
  fn from(value: ForceMergeDvUpdateOneMergeUnaryOperator) -> Self {
    Self::ForceMergeDvUpdate(value)
  }
}

#[cfg(test)]
impl<D> From<SoftUpdatesConcurrentlyOneMergeUnaryOperator> for OneMergeUnaryOperator<D>
where
  D: Directory,
{
  fn from(value: SoftUpdatesConcurrentlyOneMergeUnaryOperator) -> Self {
    Self::SoftUpdatesConcurrently(value)
  }
}

#[cfg(test)]
impl<D> From<WrappedOneMergeUnaryOperator> for OneMergeUnaryOperator<D>
where
  D: Directory,
{
  fn from(value: WrappedOneMergeUnaryOperator) -> Self {
    Self::Wrapped(value)
  }
}

#[cfg(test)]
impl<D> OneMergeUnaryOperatorBase<D> for NewOneMergeUnaryOperator
where
  D: Directory,
{
  fn apply(&self, merge: OneMergeSR<D>) -> Result<OneMergeSR<D>> {
    OneMerge::new(merge.segments)
  }
}

impl<D> Display for OneMergeWrappingMergePolicy<D>
where
  D: Directory,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "OneMergeWrappingMergePolicy({})", self.in_)
  }
}

impl<D> MergePolicy<D> for OneMergeWrappingMergePolicy<D>
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
    self.wrap_spec(
      self
        .in_
        .find_merges(merge_trigger, segment_infos, inner, merge_context)?,
    )
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
    self.wrap_spec(self.in_.find_forced_merges(
      segment_infos,
      max_segment_count,
      segments_to_merge,
      inner,
      merge_context,
    )?)
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
    self.wrap_spec(
      self
        .in_
        .find_forced_deletes_merges(segment_infos, inner, merge_context)?,
    )
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
    self.wrap_spec(self.in_.find_full_flush_merges(
      merge_trigger,
      segment_infos,
      inner,
      merge_context,
    )?)
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
