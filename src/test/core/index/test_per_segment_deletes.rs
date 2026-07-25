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
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::merge_policy::{
  DefaultMergeSpecification, MergeContext, MergePolicy, MergePolicyBase, MergePolicyEnum, OneMerge,
  size,
};
use crate::core::index::merge_trigger::MergeTrigger;
use crate::core::index::multi_terms::get_term_postings_enum_with_flag;
use crate::core::index::postings_enum::{NONE, PostingsEnum};
use crate::core::index::segment_commit_info::SegmentCommitInfo;
use crate::core::index::segment_infos::SegmentInfos;
use crate::core::index::serial_merge_scheduler::SerialMergeScheduler;
use crate::core::index::term::Term;
use crate::core::index::tiered_merge_policy::SegmentDocAndID;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::store::directory::Directory;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::index::doc_helper::DocHelper;
pub use crate::test_framework::core::index::merge_policy::RangeMergePolicy;
use crate::test_framework::core::util::lucene_test_case::{
  new_directory_shared, new_index_writer_config_with_analyzer, random,
};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::Mutex;

#[allow(dead_code)] // for quick search
struct TestPerSegmentDeletes;

#[test]
fn test_deletes1() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  iwc.set_merge_scheduler(SerialMergeScheduler::new());
  iwc.set_max_buffered_docs(5000);
  iwc.set_ram_buffer_size_mb(100.0);
  iwc.set_merge_policy(MergePolicyEnum::Range(RangeMergePolicy::new(false)));
  let writer = IndexWriter::new(dir.clone(), iwc)?;

  for x in 0..5 {
    writer.add_document(DocHelper::create_document(x, "1", 2))?;
  }
  writer.commit()?;
  assert_eq!(1, writer.clone_segment_infos()?.size());

  for x in 5..10 {
    writer.add_document(DocHelper::create_document(x, "2", 2))?;
  }
  writer.commit()?;
  assert_eq!(2, writer.clone_segment_infos()?.size());

  for x in 10..15 {
    writer.add_document(DocHelper::create_document(x, "3", 2))?;
  }

  writer.delete_documents_with_terms(vec![Term::from_text("id", "1")])?;
  writer.delete_documents_with_terms(vec![Term::from_text("id", "11")])?;
  writer.flush_with_apply_merge_deletes(false, false)?;

  // deletes are now resolved on flush, so there shouldn't be any deletes after flush
  assert!(!writer.has_changes_in_ram()?);

  // get reader flushes pending deletes so there should not be anymore
  let r1 = writer.get_reader(true, true)?;
  assert!(!writer.has_changes_in_ram()?);
  drop(r1);

  // delete id:2 from the first segment
  // merge segments 0 and 1
  // which should apply the delete id:2
  writer.delete_documents_with_terms(vec![Term::from_text("id", "2")])?;
  writer.flush_with_apply_merge_deletes(false, false)?;
  match writer.get_config_mut().get_merge_policy_mut() {
    MergePolicyEnum::Range(fsmp) => fsmp.set_merge(0, 2),
    _ => panic!("expected RangeMergePolicy"),
  }
  writer.maybe_merge()?;

  assert_eq!(2, writer.clone_segment_infos()?.size());

  // id:2 shouldn't exist anymore because
  // it's been applied in the merge and now it's gone
  let r2 = writer.get_reader(true, true)?;
  let id2docs = to_docs_array(Term::from_text("id", "2"), &r2)?;
  assert!(id2docs.is_none());
  drop(r2);

  writer.close()?;
  Ok(())
}

fn to_docs_array<IR>(term: Term, reader: IR) -> Result<Option<Vec<i32>>>
where
  IR: IndexReader,
{
  if let Some(postings_enum) =
    get_term_postings_enum_with_flag(reader, &term.field, &term.bytes, NONE as i32)?
  {
    return Ok(Some(to_array(postings_enum)?));
  }
  Ok(None)
}

fn to_array<P>(mut postings_enum: P) -> Result<Vec<i32>>
where
  P: PostingsEnum,
{
  let mut docs = Vec::new();
  while postings_enum.next_doc()? != NO_MORE_DOCS {
    docs.push(postings_enum.doc_id());
  }
  Ok(docs)
}
