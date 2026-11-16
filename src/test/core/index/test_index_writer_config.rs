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
use crate::core::analysis::analyzer::AnalyzerEnum;
use crate::core::codecs::codec::Codec;
use crate::core::document::document::Document;
use crate::core::document::field::Store;
use crate::core::document::field_type::FieldType;
use crate::core::index::flush_policy::FlushPolicyEnum;
use crate::core::index::index_deletion_policy::IndexDeletionPolicyEnum;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::{
  DEFAULT_MAX_BUFFERED_DELETE_TERMS, DEFAULT_MAX_BUFFERED_DOCS, DEFAULT_RAM_BUFFER_SIZE_MB,
  DEFAULT_RAM_PER_THREAD_HARD_LIMIT_MB, DEFAULT_READER_POOLING, DEFAULT_USE_COMPOUND_FILE_SYSTEM,
  DISABLE_AUTO_FLUSH, IndexWriterConfig, OpenMode,
};
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::merge_policy::{MergePolicy, MergePolicyEnum};
use crate::core::index::merge_scheduler::MergeSchedulerEnum;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::store::directory::{DirEnum, Directory};
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::info_stream::InfoStreamEnum;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::util::lucene_test_case::{
  new_directory_shared, new_log_merge_policy_with_cfs, new_string_field, random,
};
use std::collections::HashMap;

#[allow(dead_code)] // for quick search
struct TestIndexWriterConfig;

#[test]
fn test_defaults() -> Result<()> {
  let mut random = random();
  let conf = IndexWriterConfig::<DirEnum>::with_analyzer(MockAnalyzer::new(&mut random))?;
  assert!(matches!(conf.get_analyzer(), AnalyzerEnum::Custom(_)));
  assert!(matches!(
    conf.get_index_deletion_policy(),
    IndexDeletionPolicyEnum::KeepOnlyLastCommit(_)
  ));
  assert!(matches!(
    conf.get_merge_scheduler(),
    MergeSchedulerEnum::Concurrent(_)
  ));
  assert_eq!(&OpenMode::CreateOrAppend, conf.get_open_mode());
  // we don't need to assert this, it should be unspecified
  assert_eq!(DEFAULT_RAM_BUFFER_SIZE_MB, conf.get_ram_buffer_size_mb());
  assert_eq!(DEFAULT_MAX_BUFFERED_DOCS, conf.get_max_buffered_docs());
  assert_eq!(DEFAULT_READER_POOLING, conf.get_reader_pooling());
  assert!(conf.get_merged_segment_warmer().is_none());
  assert!(matches!(
    &*conf.get_merge_policy(),
    MergePolicyEnum::Tiered(_)
  ));
  assert!(matches!(
    conf.get_flush_policy(),
    FlushPolicyEnum::FlushByRamOrCounts(_)
  ));
  assert_eq!(
    DEFAULT_RAM_PER_THREAD_HARD_LIMIT_MB,
    conf.get_ram_per_thread_hard_limit_mb()
  );
  assert_eq!("Lucene101", conf.get_codec().get_name());
  assert!(matches!(
    conf.get_info_stream().as_ref(),
    InfoStreamEnum::NoOutput(_)
  ));
  assert_eq!(
    DEFAULT_USE_COMPOUND_FILE_SYSTEM,
    conf.get_use_compound_file()
  );
  assert!(conf.get_check_pending_flush_on_update());
  assert!(conf.get_soft_deletes_field().is_none());
  Ok(())
}

#[test]
#[ignore = "Java-only: setter bridge methods and return types are inspected through reflection"]
fn test_setters_chaining() -> Result<()> {
  test_not_required_in_rust_lucene!();
}

#[test]
#[ignore = "Java-only: Rust transfers IndexWriterConfig ownership into IndexWriter"]
fn test_reuse() -> Result<()> {
  test_not_required_in_rust_lucene!();
}

#[test]
#[ignore = "Java-only: subclass getter declarations are inspected through reflection"]
fn test_override_getters() -> Result<()> {
  test_not_required_in_rust_lucene!();
}

#[test]
#[allow(clippy::assertions_on_constants)]
fn test_constants() {
  // Tests that the values of the constants does not change
  assert_eq!(-1, DISABLE_AUTO_FLUSH);
  assert_eq!(DISABLE_AUTO_FLUSH, DEFAULT_MAX_BUFFERED_DELETE_TERMS);
  assert_eq!(DISABLE_AUTO_FLUSH, DEFAULT_MAX_BUFFERED_DOCS);
  assert_eq!(16.0, DEFAULT_RAM_BUFFER_SIZE_MB);
  assert!(DEFAULT_READER_POOLING);
  assert!(DEFAULT_USE_COMPOUND_FILE_SYSTEM);
}

#[test]
#[ignore = "Java-only: private Java fields are enumerated through reflection"]
fn test_to_string() -> Result<()> {
  test_not_required_in_rust_lucene!();
}

#[test]
fn test_invalid_values() -> Result<()> {
  // Null object-setter arguments are prevented by the type system. Numeric setter
  // validation cannot be expressed until the live-config setters return Result.
  test_not_required_in_rust_lucene!();
}

#[test]
fn test_live_change_to_cfs() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut iwc = IndexWriterConfig::with_analyzer(MockAnalyzer::new(&mut random))?;
  iwc.set_merge_policy(new_log_merge_policy_with_cfs(&mut random, true)?);
  // Start false:
  iwc.set_use_compound_file(false);
  iwc
    .get_merge_policy_mut()
    .get_base_mut()
    .set_no_cfs_ratio(0.0)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;
  // Change to true:
  w.get_config()?.set_use_compound_file(true);

  let mut field_to_type = HashMap::<String, FieldType>::new();
  let mut doc = Document::new();
  doc.add(new_string_field(
    &mut random,
    "field",
    "foo",
    Store::No,
    &mut field_to_type,
  )?);
  w.add_document(&mut doc)?;
  w.commit()?;
  assert!(w.newest_segment().unwrap().info.get_use_compound_file());

  doc.add(new_string_field(
    &mut random,
    "field",
    "foo",
    Store::No,
    &mut field_to_type,
  )?);
  w.add_document(&mut doc)?;
  w.commit()?;
  w.force_merge(1)?;
  w.commit()?;

  // no compound files after merge
  assert!(!w.newest_segment().unwrap().info.get_use_compound_file());

  let mut lmp = w.get_config()?.get_merge_policy_mut();
  lmp.get_base_mut().set_no_cfs_ratio(1.0)?;
  lmp
    .get_base_mut()
    .set_max_cfs_segment_size_mb(f64::INFINITY)?;
  drop(lmp);

  w.add_document(&mut doc)?;
  w.force_merge(1)?;
  w.commit()?;
  assert!(w.newest_segment().unwrap().info.get_use_compound_file());
  w.close()?;
  dir.close()
}
