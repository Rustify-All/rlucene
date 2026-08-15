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
#[cfg(test)]
use crate::core::codecs::CodecLiveDocsBits;
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_writer::Inner;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::merge_policy::{
  DefaultMergeSpecification, MergeContext, MergePolicy, MergePolicyBase, MergePolicyEnum,
  MergeSpecification, MergeStat, OneMergeBase, OneMergeDefaults, OneMergeHook, OneMergeSR,
};
use crate::core::index::merge_trigger::MergeTrigger;
use crate::core::index::one_merge_wrapping_merge_policy::{
  OneMergeUnaryOperator, OneMergeUnaryOperatorBase, OneMergeWrappingMergePolicy,
};
use crate::core::index::pending_deletes::DocBits;
use crate::core::index::segment_commit_info::SegmentCommitInfo;
use crate::core::index::segment_infos::SegmentInfos;
use crate::core::index::segment_reader::{DefaultLeafReader, SegmentReader};
use crate::core::search::boolean_clause::Occur;
use crate::core::search::boolean_query::Builder;
use crate::core::search::doc_id_set_iterator::{DocIdSetIterator, NO_MORE_DOCS};
use crate::core::search::field_exists_query::FieldExistsQuery;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::query::Query;
use crate::core::search::score_mode::ScoreMode;
use crate::core::store::directory::Directory;
use crate::core::util::bit_set::BitSet;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::io_utils::IOUtils;
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

pub type RetentionQuerySupplier = Arc<dyn Fn() -> Result<Query> + Send + Sync>;

/// This [`MergePolicy`] allows soft-deleted documents to be carried over across merges. The policy
/// wraps the merge reader and marks documents as "live" that have a value in the soft-delete field
/// and match the provided query. This allows, for instance, documents to be kept alive based on
/// time or any other constraint in the index. The main purpose of this merge policy is to implement
/// retention policies that control when document modifications vanish from the index. Using this
/// merge policy allows control over when soft deletes are claimed by merges.
///
/// # Experimental
///
/// This API is experimental and may change in incompatible ways.
pub struct SoftDeletesRetentionMergePolicy<D>
where
  D: Directory,
{
  base: OneMergeWrappingMergePolicy<D>,
  field: String,
  retention_query_supplier: RetentionQuerySupplier,
  matching_docs: fn(Query, DefaultLeafReader<D>) -> Result<Option<FixedBitSet>>,
}

impl<D> Clone for SoftDeletesRetentionMergePolicy<D>
where
  D: Directory,
{
  fn clone(&self) -> Self {
    Self {
      base: self.base.clone(),
      field: self.field.clone(),
      retention_query_supplier: self.retention_query_supplier.clone(),
      matching_docs: self.matching_docs,
    }
  }
}

impl<D> SoftDeletesRetentionMergePolicy<D>
where
  D: Directory,
{
  /// Creates a new [`SoftDeletesRetentionMergePolicy`].
  ///
  /// * `field` — the soft-delete field
  /// * `retention_query_supplier` — a supplier for the retention query
  /// * `in_` — the wrapped merge policy
  pub fn new<T, S>(field: impl Into<String>, retention_query_supplier: S, in_: T) -> Self
  where
    D: 'static,
    T: Into<MergePolicyEnum<D>>,
    S: Fn() -> Result<Query> + Send + Sync + 'static,
  {
    let field = field.into();
    let retention_query_supplier: RetentionQuerySupplier = Arc::new(retention_query_supplier);
    let operator = SoftDeletesRetentionOneMergeUnaryOperator::new(
      field.clone(),
      retention_query_supplier.clone(),
      apply_retention_query::<D>,
    );
    Self {
      base: OneMergeWrappingMergePolicy::new(in_, operator),
      field,
      retention_query_supplier,
      matching_docs: matching_docs::<D>,
    }
  }
}

impl<D> Display for SoftDeletesRetentionMergePolicy<D>
where
  D: Directory,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "SoftDeletesRetentionMergePolicy({})", self.base)
  }
}

impl<D> MergePolicy<D> for SoftDeletesRetentionMergePolicy<D>
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
    let reader = reader_supplier()?;
    let max_doc = reader.max_doc()?;
    let all_docs_reader = new_reader_with_live_docs(reader, None, max_doc, None)?;
    let matches = (self.matching_docs)((self.retention_query_supplier)()?, all_docs_reader)?;
    // We only need a single hit to keep it; there is no need for soft deletes to be checked.
    if let Some(matches) = matches
      && matches.cardinality() > 0
    {
      return Ok(true);
    }
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
    let num_deletes_to_merge = self
      .base
      .num_deletes_to_merge(info, del_count, reader_supplier)?;
    if num_deletes_to_merge != 0 && info.get_soft_del_count() > 0 {
      let reader = reader_supplier()?;
      if let Some(live_docs) = reader.get_live_docs()? {
        let max_doc = reader.max_doc()?;
        let num_deleted_docs = reader.num_deleted_docs()?;
        let all_docs_reader = new_reader_with_live_docs(reader, None, max_doc, None)?;
        let query = build_retention_query(&self.field, (self.retention_query_supplier)()?)?;
        let matches = (self.matching_docs)(query, all_docs_reader)?;
        if let Some(matches) = matches {
          let mut num_deleted_docs = num_deleted_docs;
          let mut doc_id = matches.next_set_bit(0);
          while doc_id != NO_MORE_DOCS as usize {
            if !live_docs.get(doc_id)? {
              num_deleted_docs -= 1;
            }
            doc_id = if doc_id + 1 >= max_doc as usize {
              NO_MORE_DOCS as usize
            } else {
              matches.next_set_bit(doc_id + 1)
            };
          }
          return Ok(num_deleted_docs);
        }
      }
    }
    debug_assert!(num_deletes_to_merge >= 0);
    debug_assert!(num_deletes_to_merge <= info.info.max_doc()?);
    Ok(num_deletes_to_merge)
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

pub struct SoftDeletesRetentionOneMergeUnaryOperator<D>
where
  D: Directory,
{
  field: String,
  retention_query_supplier: RetentionQuerySupplier,
  apply_retention_query: fn(&str, Query, DefaultLeafReader<D>) -> Result<DefaultLeafReader<D>>,
}

impl<D> Clone for SoftDeletesRetentionOneMergeUnaryOperator<D>
where
  D: Directory,
{
  fn clone(&self) -> Self {
    Self {
      field: self.field.clone(),
      retention_query_supplier: self.retention_query_supplier.clone(),
      apply_retention_query: self.apply_retention_query,
    }
  }
}

impl<D> SoftDeletesRetentionOneMergeUnaryOperator<D>
where
  D: Directory,
{
  fn new(
    field: String,
    retention_query_supplier: RetentionQuerySupplier,
    apply_retention_query: fn(&str, Query, DefaultLeafReader<D>) -> Result<DefaultLeafReader<D>>,
  ) -> Self {
    Self {
      field,
      retention_query_supplier,
      apply_retention_query,
    }
  }
}

impl<D> From<SoftDeletesRetentionOneMergeUnaryOperator<D>> for OneMergeUnaryOperator<D>
where
  D: Directory,
{
  fn from(value: SoftDeletesRetentionOneMergeUnaryOperator<D>) -> Self {
    Self::SoftDeletesRetention(value)
  }
}

impl<D> OneMergeUnaryOperatorBase<D> for SoftDeletesRetentionOneMergeUnaryOperator<D>
where
  D: Directory,
{
  fn apply(&self, mut merge: OneMergeSR<D>) -> Result<OneMergeSR<D>> {
    let wrapped = merge.replace_hook(OneMergeHook::Default);
    merge.replace_hook(OneMergeHook::SoftDeletesRetention(
      SoftDeletesRetentionOneMerge::new(
        wrapped,
        self.field.clone(),
        self.retention_query_supplier.clone(),
        self.apply_retention_query,
      ),
    ));
    Ok(merge)
  }
}

pub(crate) struct SoftDeletesRetentionOneMerge<D, CR>
where
  D: Directory,
  CR: CodecReader,
{
  wrapped: Box<OneMergeHook<D, CR>>,
  field: String,
  retention_query_supplier: RetentionQuerySupplier,
  apply_retention_query: fn(&str, Query, CR) -> Result<CR>,
}

impl<D, CR> SoftDeletesRetentionOneMerge<D, CR>
where
  D: Directory,
  CR: CodecReader,
{
  fn new(
    wrapped: OneMergeHook<D, CR>,
    field: String,
    retention_query_supplier: RetentionQuerySupplier,
    apply_retention_query: fn(&str, Query, CR) -> Result<CR>,
  ) -> Self {
    Self {
      wrapped: Box::new(wrapped),
      field,
      retention_query_supplier,
      apply_retention_query,
    }
  }
}

impl<D, CR> OneMergeBase<D, CR> for SoftDeletesRetentionOneMerge<D, CR>
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
    let has_deletions = reader.get_live_docs()?.is_some();
    let wrapped = self.wrapped.wrap_for_merge(reader)?;
    if has_deletions {
      (self.apply_retention_query)(&self.field, (self.retention_query_supplier)()?, wrapped)
    } else {
      Ok(wrapped)
    }
  }

  fn reorder<CR1, D1>(
    &self,
    reader: &CR1,
    dir: D1,
  ) -> Result<Option<crate::core::index::dummy::dummy_doc_map_sorter::DummyDocMap>>
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
    merge_readers: &mut Vec<crate::core::index::merge_policy::MergeReader<CR>>,
    stat: &MergeStat,
    reader_factory: F,
  ) -> Result<()>
  where
    F: FnMut(&String) -> Result<crate::core::index::merge_policy::MergeReader<CR>>,
  {
    OneMergeDefaults::init_merge_readers(merge_readers, stat, reader_factory)
  }
}

fn apply_retention_query<D>(
  soft_delete_field: &str,
  retention_query: Query,
  reader: DefaultLeafReader<D>,
) -> Result<DefaultLeafReader<D>>
where
  D: Directory + 'static,
{
  let live_docs = match reader.get_live_docs()? {
    Some(live_docs) => live_docs,
    None => return Ok(reader), // no deletes - just keep going
  };
  let max_doc = reader.max_doc()?;
  // Only search deleted documents.
  let mut deleted_docs = FixedBitSet::new(max_doc as usize);
  for doc_id in 0..max_doc as usize {
    if !live_docs.get(doc_id)? {
      deleted_docs.set(doc_id);
    }
  }
  let deleted_count = reader.num_deleted_docs()?;
  let deleted_reader = new_reader_with_live_docs(
    reader.clone(),
    Some(deleted_docs),
    deleted_count,
    reader.get_hard_live_docs()?,
  )?;
  let query = build_retention_query(soft_delete_field, retention_query)?;
  let Some(retained_docs) = matching_docs(query, deleted_reader)? else {
    return Ok(reader);
  };
  let mut new_live_docs = live_docs.copy_of()?;
  let mut extra_live_docs = 0;
  let mut doc_id = retained_docs.next_set_bit(0);
  while doc_id != NO_MORE_DOCS as usize {
    if !new_live_docs.get(doc_id)? {
      new_live_docs.set(doc_id);
      // If we bring one back to live, we need to account for it.
      extra_live_docs += 1;
    }
    doc_id = if doc_id + 1 >= max_doc as usize {
      NO_MORE_DOCS as usize
    } else {
      retained_docs.next_set_bit(doc_id + 1)
    };
  }
  debug_assert!(reader.num_docs()? + extra_live_docs <= max_doc);
  let num_docs = reader.num_docs()? + extra_live_docs;
  let hard_live_docs = reader.get_hard_live_docs()?;
  new_reader_with_live_docs(reader, Some(new_live_docs), num_docs, hard_live_docs)
}

fn build_retention_query(soft_delete_field: &str, retention_query: Query) -> Result<Query> {
  let mut builder = Builder::new();
  builder.add(FieldExistsQuery::new(soft_delete_field), Occur::Filter)?;
  builder.add(retention_query, Occur::Filter)?;
  Ok(builder.build().into())
}

fn matching_docs<D>(query: Query, reader: DefaultLeafReader<D>) -> Result<Option<FixedBitSet>>
where
  D: Directory + 'static,
{
  let max_doc = reader.max_doc()?;
  let mut searcher = IndexSearcher::new(reader.clone().get_context()?)?;
  searcher.set_query_cache(None);
  let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
    || -> Result<Option<FixedBitSet>> {
      let query = searcher.rewrite(query)?;
      let weight = searcher.create_weight(query, ScoreMode::CompleteNoScores, 1.0)?;
      let context = &searcher.get_leaf_contexts()?[0];
      let mut matches = FixedBitSet::new(max_doc as usize);
      if let Some(mut scorer) = weight.scorer(context, &searcher)? {
        loop {
          let doc_id = scorer.iterator_mut().next_doc()?;
          if doc_id == NO_MORE_DOCS {
            break;
          }
          matches.set(doc_id as usize);
        }
        Ok(Some(matches))
      } else {
        Ok(None)
      }
    },
  ));
  drop(searcher);
  let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| reader.dec_ref()));
  IOUtils::use_or_suppress_caught_result(result, close_result)
}

fn new_reader_with_live_docs<D>(
  reader: DefaultLeafReader<D>,
  live_docs: Option<FixedBitSet>,
  num_docs: i32,
  hard_live_docs: Option<DocBits>,
) -> Result<DefaultLeafReader<D>>
where
  D: Directory,
{
  let live_docs = live_docs.map(|bits| {
    #[cfg(not(test))]
    {
      Arc::new(bits.to_read_only_bits())
    }
    #[cfg(test)]
    {
      Arc::new(CodecLiveDocsBits::Lucene90(bits.to_read_only_bits()))
    }
  });
  Ok(Arc::new(SegmentReader::new_from_reader(
    reader.get_segment_info(),
    reader.as_ref(),
    live_docs,
    hard_live_docs,
    num_docs,
    true,
  )?))
}
