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
use super::test_pending_deletes::TestPendingDeletesBase;
use crate::core::codecs::codec;
use crate::core::document::document::Document;
use crate::core::document::field::Store;
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::document::string_field::StringField;
use crate::core::index::directory_reader;
use crate::core::index::doc_values_field_updates::{
  DocValuesFieldIteratorEnum, DocValuesFieldUpdates, merged_iterator,
};
use crate::core::index::doc_values_skip_index_type::DocValuesSkipIndexType;
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::{DISABLE_AUTO_FLUSH, IndexWriterConfig};
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::no_merge_policy::NoMergePolicy;
use crate::core::index::pending_deletes::{PendingDeletesBase, PendingDeletesEnum};
use crate::core::index::pending_soft_deletes::PendingSoftDeletes;
use crate::core::index::segment_commit_info::SegmentCommitInfo;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::term::Term;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::index::vector_encoding::VectorEncoding;
use crate::core::index::vector_similarity_function::VectorSimilarityFunction;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::store::directory::Directory;
use crate::core::util::HasIdentity;
use crate::core::util::bits::Bits;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::Result;
use crate::core::util::{LATEST, StringHelper};
use crate::test_framework::core::index::test_pending_soft_deletes::TestSingleUpdateDocValuesFieldUpdates;
use crate::test_framework::core::util::lucene_test_case::{new_directory_shared, random};
use std::collections::HashMap;
use std::sync::Arc;

#[allow(dead_code)] // for quick search
struct TestPendingSoftDeletes;

mod test_pending_deletes_base_tests {
  use super::super::test_pending_deletes::TestPendingDeletesBase;
  use super::TestPendingSoftDeletes;
  use crate::core::util::error::lucene_error::Result;
  use crate::test_framework::core::util::lucene_test_case::random;

  #[test]
  fn test_delete_doc() -> Result<()> {
    let mut random = random();
    let case = TestPendingSoftDeletes;
    case.test_delete_doc(&mut random)
  }

  #[test]
  fn test_write_live_docs() -> Result<()> {
    let mut random = random();
    let case = TestPendingSoftDeletes;
    case.test_write_live_docs(&mut random)
  }

  #[test]
  fn test_is_fully_deleted() -> Result<()> {
    let mut random = random();
    let case = TestPendingSoftDeletes;
    case.test_is_fully_deleted(&mut random)
  }
}

impl TestPendingDeletesBase for TestPendingSoftDeletes {
  fn new_pending_deletes<D>(&self, commit_info: &SegmentCommitInfo<D>) -> Result<PendingDeletesEnum>
  where
    D: Directory,
  {
    Ok(PendingDeletesEnum::Soft(PendingSoftDeletes::new(
      Arc::from("_soft_deletes"),
      commit_info,
    )?))
  }
}

#[test]
fn test_hard_delete_soft_deleted() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut config = IndexWriterConfig::new()?;
  config
    .set_soft_deletes_field("_soft_deletes")
    // make sure all docs will end up in the same segment
    .set_max_buffered_docs(10)
    .set_merge_policy(NoMergePolicy::default())
    .set_ram_buffer_size_mb(DISABLE_AUTO_FLUSH as f64);
  let writer = IndexWriter::new(dir.clone(), config)?;
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "1", Store::Yes)?);
  writer.soft_update_document(
    Term::from_text("id", "1"),
    doc,
    vec![NumericDocValuesField::new("_soft_deletes", 1).into()],
  )?;
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "2", Store::Yes)?);
  writer.soft_update_document(
    Term::from_text("id", "2"),
    doc,
    vec![NumericDocValuesField::new("_soft_deletes", 1).into()],
  )?;
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "2", Store::Yes)?);
  writer.soft_update_document(
    Term::from_text("id", "2"),
    doc,
    vec![NumericDocValuesField::new("_soft_deletes", 1).into()],
  )?;
  writer.commit()?;
  let reader = directory_reader::open(dir.clone())?;
  let context = (&reader).get_context()?;
  let leaves = context.leaves()?;
  assert_eq!(1, leaves.len());
  let segment_reader = leaves[0].reader();
  let segment_info: &SegmentCommitInfo<_> = segment_reader.get_segment_info();
  let mut pending_soft_deletes = match TestPendingSoftDeletes.new_pending_deletes(segment_info)? {
    PendingDeletesEnum::Soft(deletes) => deletes,
    PendingDeletesEnum::PD(_) => unreachable!(),
  };
  pending_soft_deletes.on_new_reader(segment_reader, segment_info)?;
  assert_eq!(0, pending_soft_deletes.num_pending_deletes());
  assert_eq!(1, pending_soft_deletes.get_del_count(segment_info));
  let live_docs = pending_soft_deletes.get_live_docs().unwrap();
  assert!(live_docs.get(0)?);
  assert!(!live_docs.get(1)?);
  assert!(live_docs.get(2)?);
  assert!(pending_soft_deletes.get_hard_live_docs().is_none());
  assert!(pending_soft_deletes.delete(1, segment_info)?);
  assert_eq!(0, pending_soft_deletes.num_pending_deletes());
  assert_eq!(-1, pending_soft_deletes.base.pending_delete_count); // transferred the delete
  assert_eq!(1, pending_soft_deletes.get_del_count(segment_info));
  reader.close()?;
  writer.close()?;
  dir.close()?;
  Ok(())
}

#[test]
fn test_delete_soft() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut config = IndexWriterConfig::new()?;
  config
    .set_soft_deletes_field("_soft_deletes")
    // make sure all docs will end up in the same segment
    .set_max_buffered_docs(10)
    .set_merge_policy(NoMergePolicy::default())
    .set_ram_buffer_size_mb(DISABLE_AUTO_FLUSH as f64);
  let writer = IndexWriter::new(dir.clone(), config)?;
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "1", Store::Yes)?);
  writer.soft_update_document(
    Term::from_text("id", "1"),
    doc,
    vec![NumericDocValuesField::new("_soft_deletes", 1).into()],
  )?;
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "2", Store::Yes)?);
  writer.soft_update_document(
    Term::from_text("id", "2"),
    doc,
    vec![NumericDocValuesField::new("_soft_deletes", 1).into()],
  )?;
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "2", Store::Yes)?);
  writer.soft_update_document(
    Term::from_text("id", "2"),
    doc,
    vec![NumericDocValuesField::new("_soft_deletes", 1).into()],
  )?;
  writer.commit()?;
  let mut reader = directory_reader::open(dir.clone())?;
  let context = (&reader).get_context()?;
  let leaves = context.leaves()?;
  assert_eq!(1, leaves.len());
  let segment_reader = leaves[0].reader();
  let segment_info: &SegmentCommitInfo<_> = segment_reader.get_segment_info();
  let mut pending_soft_deletes = TestPendingSoftDeletes.new_pending_deletes(segment_info)?;
  pending_soft_deletes.on_new_reader(segment_reader, segment_info)?;
  assert_eq!(0, pending_soft_deletes.num_pending_deletes());
  assert_eq!(1, pending_soft_deletes.get_del_count(segment_info));
  let live_docs = pending_soft_deletes.get_live_docs().unwrap();
  assert!(live_docs.get(0)?);
  assert!(!live_docs.get(1)?);
  assert!(live_docs.get(2)?);
  assert!(pending_soft_deletes.get_hard_live_docs().is_none());
  // pass reader again
  let live_docs = pending_soft_deletes.get_live_docs().unwrap();
  pending_soft_deletes.on_new_reader(segment_reader, segment_info)?;
  assert_eq!(0, pending_soft_deletes.num_pending_deletes());
  assert_eq!(1, pending_soft_deletes.get_del_count(segment_info));
  let same_live_docs = pending_soft_deletes.get_live_docs().unwrap();
  assert_eq!(live_docs.identity(), same_live_docs.identity());

  // now apply a hard delete
  writer.delete_documents_with_terms(vec![Term::from_text("id", "1")])?;
  writer.commit()?;
  reader.close()?;
  reader = directory_reader::open(dir.clone())?;
  let context = (&reader).get_context()?;
  let leaves = context.leaves()?;
  assert_eq!(1, leaves.len());
  let segment_reader = leaves[0].reader();
  let segment_info: &SegmentCommitInfo<_> = segment_reader.get_segment_info();
  pending_soft_deletes = TestPendingSoftDeletes.new_pending_deletes(segment_info)?;
  pending_soft_deletes.on_new_reader(segment_reader, segment_info)?;
  assert_eq!(0, pending_soft_deletes.num_pending_deletes());
  assert_eq!(2, pending_soft_deletes.get_del_count(segment_info));
  let live_docs = pending_soft_deletes.get_live_docs().unwrap();
  assert!(!live_docs.get(0)?);
  assert!(!live_docs.get(1)?);
  assert!(live_docs.get(2)?);
  let hard_live_docs = pending_soft_deletes.get_hard_live_docs();
  assert!(hard_live_docs.is_some());
  let hard_live_docs = hard_live_docs.unwrap();
  assert!(!hard_live_docs.get(0)?);
  assert!(hard_live_docs.get(1)?);
  assert!(hard_live_docs.get(2)?);
  reader.close()?;
  writer.close()?;
  dir.close()?;
  Ok(())
}

#[test]
fn test_apply_updates() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let si = SegmentInfo::new(
    dir.clone(),
    Some((*LATEST).clone()),
    Some((*LATEST).clone()),
    "test",
    10,
    false,
    false,
    Some(codec::get_default()),
    HashMap::new(),
    StringHelper::random_id(),
    HashMap::new(),
    None,
  )?;
  let mut commit_info =
    SegmentCommitInfo::new(si, 0, 0, -1, -1, -1, Some(StringHelper::random_id()));
  let writer = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;
  for _ in 0..commit_info.info.max_doc()? {
    writer.add_document(Document::new())?;
  }
  writer.force_merge(1)?;
  writer.commit()?;
  let reader = directory_reader::open_from_writer(&writer)?;
  let context = (&reader).get_context()?;
  let leaves = context.leaves()?;
  assert_eq!(1, leaves.len());
  let segment_reader = leaves[0].reader();
  let mut deletes = TestPendingSoftDeletes.new_pending_deletes(&commit_info)?;
  deletes.on_new_reader(segment_reader, &commit_info)?;
  reader.close()?;
  writer.close()?;
  let field_info = FieldInfo::new(
    "_soft_deletes",
    1,
    false,
    false,
    false,
    IndexOptions::None,
    DocValuesType::Numeric,
    DocValuesSkipIndexType::None,
    0,
    HashMap::new(),
    0,
    0,
    0,
    0,
    VectorEncoding::FLOAT32(4),
    VectorSimilarityFunction::Euclidean,
    true,
    false,
  )?;
  let docs_deleted = [1, 3, 7, 8, NO_MORE_DOCS];
  let updates = vec![single_update(&docs_deleted, 10, true)?];
  for update in updates {
    deletes.on_doc_values_update(&field_info, update, &mut commit_info)?;
  }
  assert_eq!(0, deletes.num_pending_deletes());
  assert_eq!(4, deletes.get_del_count(&commit_info));
  let live_docs = deletes.get_live_docs().unwrap();
  assert!(live_docs.get(0)?);
  assert!(!live_docs.get(1)?);
  assert!(live_docs.get(2)?);
  assert!(!live_docs.get(3)?);
  assert!(live_docs.get(4)?);
  assert!(live_docs.get(5)?);
  assert!(live_docs.get(6)?);
  assert!(!live_docs.get(7)?);
  assert!(!live_docs.get(8)?);
  assert!(live_docs.get(9)?);

  let docs_deleted = [1, 2, NO_MORE_DOCS];
  let updates = vec![single_update(&docs_deleted, 10, true)?];
  let field_info = FieldInfo::new(
    "_soft_deletes",
    1,
    false,
    false,
    false,
    IndexOptions::None,
    DocValuesType::Numeric,
    DocValuesSkipIndexType::None,
    1,
    HashMap::new(),
    0,
    0,
    0,
    0,
    VectorEncoding::FLOAT32(4),
    VectorSimilarityFunction::Euclidean,
    true,
    false,
  )?;
  for update in updates {
    deletes.on_doc_values_update(&field_info, update, &mut commit_info)?;
  }
  assert_eq!(0, deletes.num_pending_deletes());
  assert_eq!(5, deletes.get_del_count(&commit_info));
  let live_docs = deletes.get_live_docs().unwrap();
  assert!(live_docs.get(0)?);
  assert!(!live_docs.get(1)?);
  assert!(!live_docs.get(2)?);
  assert!(!live_docs.get(3)?);
  assert!(live_docs.get(4)?);
  assert!(live_docs.get(5)?);
  assert!(live_docs.get(6)?);
  assert!(!live_docs.get(7)?);
  assert!(!live_docs.get(8)?);
  assert!(live_docs.get(9)?);
  dir.close()?;
  Ok(())
}

#[test]
fn test_update_applied_only_once() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut config = IndexWriterConfig::new()?;
  config
    .set_soft_deletes_field("_soft_deletes")
    .set_max_buffered_docs(3) // make sure we write one segment
    .set_merge_policy(NoMergePolicy::default()) // prevent deletes from triggering merges
    .set_ram_buffer_size_mb(DISABLE_AUTO_FLUSH as f64);
  let writer = IndexWriter::new(dir.clone(), config)?;
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "1", Store::Yes)?);
  writer.soft_update_document(
    Term::from_text("id", "1"),
    doc,
    vec![NumericDocValuesField::new("_soft_deletes", 1).into()],
  )?;
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "2", Store::Yes)?);
  writer.soft_update_document(
    Term::from_text("id", "2"),
    doc,
    vec![NumericDocValuesField::new("_soft_deletes", 1).into()],
  )?;
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "2", Store::Yes)?);
  writer.soft_update_document(
    Term::from_text("id", "2"),
    doc,
    vec![NumericDocValuesField::new("_soft_deletes", 1).into()],
  )?;
  writer.commit()?;
  let reader = directory_reader::open(dir.clone())?;
  let context = (&reader).get_context()?;
  let leaves = context.leaves()?;
  assert_eq!(1, leaves.len());
  let segment_reader = leaves[0].reader();
  let mut segment_info = segment_reader.get_segment_info().clone();
  let mut deletes = TestPendingSoftDeletes.new_pending_deletes(&segment_info)?;
  deletes.on_new_reader(segment_reader, &segment_info)?;
  let field_info = FieldInfo::new(
    "_soft_deletes",
    1,
    false,
    false,
    false,
    IndexOptions::None,
    DocValuesType::Numeric,
    DocValuesSkipIndexType::None,
    segment_info.get_next_doc_values_gen(),
    HashMap::new(),
    0,
    0,
    0,
    0,
    VectorEncoding::FLOAT32(4),
    VectorSimilarityFunction::Euclidean,
    true,
    false,
  )?;
  let docs_deleted = [1, NO_MORE_DOCS];
  let updates = vec![single_update(&docs_deleted, 3, true)?];
  for update in updates {
    deletes.on_doc_values_update(&field_info, update, &mut segment_info)?;
  }
  assert_eq!(0, deletes.num_pending_deletes());
  assert_eq!(1, deletes.get_del_count(&segment_info));
  let live_docs = deletes.get_live_docs().unwrap();
  assert!(live_docs.get(0)?);
  assert!(!live_docs.get(1)?);
  assert!(live_docs.get(2)?);
  let live_docs = deletes.get_live_docs().unwrap();
  deletes.on_new_reader(segment_reader, &segment_info)?;
  // no changes we don't apply updates twice
  let same_live_docs = deletes.get_live_docs().unwrap();
  assert_eq!(live_docs.identity(), same_live_docs.identity());
  let live_docs = deletes.get_live_docs().unwrap();
  assert!(live_docs.get(0)?);
  assert!(!live_docs.get(1)?);
  assert!(live_docs.get(2)?);
  assert_eq!(0, deletes.num_pending_deletes());
  assert_eq!(1, deletes.get_del_count(&segment_info));
  reader.close()?;
  writer.close()?;
  dir.close()?;
  Ok(())
}

#[test]
fn test_reset_on_update() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut config = IndexWriterConfig::new()?;
  config
    .set_soft_deletes_field("_soft_deletes")
    .set_max_buffered_docs(3) // make sure we write one segment
    .set_merge_policy(NoMergePolicy::default()) // prevent deletes from triggering merges
    .set_ram_buffer_size_mb(DISABLE_AUTO_FLUSH as f64);
  let writer = IndexWriter::new(dir.clone(), config)?;
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "1", Store::Yes)?);
  writer.soft_update_document(
    Term::from_text("id", "1"),
    doc,
    vec![NumericDocValuesField::new("_soft_deletes", 1).into()],
  )?;
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "2", Store::Yes)?);
  writer.soft_update_document(
    Term::from_text("id", "2"),
    doc,
    vec![NumericDocValuesField::new("_soft_deletes", 1).into()],
  )?;
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "2", Store::Yes)?);
  writer.soft_update_document(
    Term::from_text("id", "2"),
    doc,
    vec![NumericDocValuesField::new("_soft_deletes", 1).into()],
  )?;
  writer.commit()?;
  let reader = directory_reader::open(dir.clone())?;
  let context = (&reader).get_context()?;
  let leaves = context.leaves()?;
  assert_eq!(1, leaves.len());
  let segment_reader = leaves[0].reader();
  let mut segment_info = segment_reader.get_segment_info().clone();
  let mut deletes = TestPendingSoftDeletes.new_pending_deletes(&segment_info)?;
  deletes.on_new_reader(segment_reader, &segment_info)?;
  let field_info = FieldInfo::new(
    "_soft_deletes",
    1,
    false,
    false,
    false,
    IndexOptions::None,
    DocValuesType::Numeric,
    DocValuesSkipIndexType::None,
    segment_info.get_next_doc_values_gen(),
    HashMap::new(),
    0,
    0,
    0,
    0,
    VectorEncoding::FLOAT32(4),
    VectorSimilarityFunction::Euclidean,
    true,
    false,
  )?;
  let updates = vec![single_update(&[0, 1, NO_MORE_DOCS], 3, false)?];
  for update in updates {
    deletes.on_doc_values_update(&field_info, update, &mut segment_info)?;
  }
  assert_eq!(0, deletes.num_pending_deletes());
  let live_docs = deletes.get_live_docs().unwrap();
  assert!(live_docs.get(0)?);
  assert!(live_docs.get(1)?);
  assert!(live_docs.get(2)?);
  let live_docs = deletes.get_live_docs().unwrap();
  deletes.on_new_reader(segment_reader, &segment_info)?;
  // no changes we keep this update
  let same_live_docs = deletes.get_live_docs().unwrap();
  assert_eq!(live_docs.identity(), same_live_docs.identity());
  let live_docs = deletes.get_live_docs().unwrap();
  assert!(live_docs.get(0)?);
  assert!(live_docs.get(1)?);
  assert!(live_docs.get(2)?);
  assert_eq!(0, deletes.num_pending_deletes());

  segment_info.advance_doc_values_gen();
  let field_info = FieldInfo::new(
    "_soft_deletes",
    1,
    false,
    false,
    false,
    IndexOptions::None,
    DocValuesType::Numeric,
    DocValuesSkipIndexType::None,
    segment_info.get_next_doc_values_gen(),
    HashMap::new(),
    0,
    0,
    0,
    0,
    VectorEncoding::FLOAT32(4),
    VectorSimilarityFunction::Euclidean,
    true,
    false,
  )?;
  let updates = vec![single_update(&[1, NO_MORE_DOCS], 3, true)?];
  for update in updates {
    deletes.on_doc_values_update(&field_info, update, &mut segment_info)?;
  }
  // no changes we keep this update
  let same_live_docs = deletes.get_live_docs().unwrap();
  assert_ne!(live_docs.identity(), same_live_docs.identity());
  let live_docs = deletes.get_live_docs().unwrap();
  assert!(live_docs.get(0)?);
  assert!(!live_docs.get(1)?);
  assert!(live_docs.get(2)?);
  assert_eq!(0, deletes.num_pending_deletes());
  assert_eq!(1, deletes.get_del_count(&segment_info));
  reader.close()?;
  writer.close()?;
  dir.close()?;
  Ok(())
}

fn single_update(
  docs_changed: &[i32],
  max_doc: i32,
  has_value: bool,
) -> Result<
  Option<crate::core::index::doc_values_field_updates::MergedIterator<DocValuesFieldIteratorEnum>>,
> {
  let sub_update = TestSingleUpdateDocValuesFieldUpdates::new(docs_changed.to_vec(), has_value);
  let mut update = DocValuesFieldUpdates::new(
    max_doc,
    0,
    "_soft_deletes",
    DocValuesType::Numeric,
    sub_update,
  )?;
  update.finish()?;
  merged_iterator(vec![update.iterator()?])
}
